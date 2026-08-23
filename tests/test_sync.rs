//! Тесты синхронизации (фаза 8): миграция M2, roundtrip, LWW, tombstones,
//! идемпотентность, битые бандлы. Эмбеддинги — fake (быстро, без модели).

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::extractor::{Entity, ExtractionResult, Relation};
use ob2h::init_app;
use ob2h::mcp::AppContext;
use tempfile::tempdir;

fn make_ctx(dir: &std::path::Path, origin: &str, priority: &[&str]) -> std::sync::Arc<AppContext> {
    let sync_dir = dir.join("sync");
    std::fs::create_dir_all(&sync_dir).unwrap();
    let peers = if priority.is_empty() {
        format!("{{\"origin\": \"{origin}\"}}\n")
    } else {
        format!(
            "{{\"origin\": \"{origin}\", \"priority\": [{}]}}\n",
            priority
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    std::fs::write(sync_dir.join("peers.json"), peers).unwrap();

    let mut settings = Settings::from_env();
    settings.data_dir = dir.to_path_buf();
    settings.embed_provider = "fake".to_string();
    init_app(settings).expect("init_app")
}

fn set_mem_ts(ctx: &AppContext, key: &str, ts: &str, origin: &str) {
    ctx.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET updated_at = ?1, origin = ?2 WHERE key = ?3",
                rusqlite::params![ts, origin, key],
            )?;
            Ok(())
        })
        .unwrap();
}

/// M2 поверх живой v1-БД: колонки появляются, updated_at рёбер бэкуfilled,
/// v0.7.1-стиль INSERT (без новых колонок) продолжает работать.
#[test]
fn test_m2_migration_from_v1() {
    let tmp = tempdir().expect("tempdir");
    let db_path = tmp.path().join("ob2h.db");

    // Готовим БД уровня схемы v1 (как после v0.7.1)
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(ob2h::db::schema::MIGRATION_V1).unwrap();
        conn.execute(
            "INSERT INTO kv (key, value) VALUES ('schema_version', '1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, created_at, updated_at)
             VALUES ('old', 'факт из v0.7.1', 'general', 0.5, '2026-08-01T00:00:00+00:00', '2026-08-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (node_id, label, node_type, description, created_at, updated_at)
             VALUES ('n1', 'Нода', 'Concept', 'desc', '2026-08-01T00:00:00+00:00', '2026-08-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (node_id, label, node_type, description, created_at, updated_at)
             VALUES ('n2', 'Нода2', 'Concept', 'desc', '2026-08-01T00:00:00+00:00', '2026-08-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
        let ids: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT id FROM graph_nodes WHERE node_id='n1'), (SELECT id FROM graph_nodes WHERE node_id='n2')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, contexts, created_at)
             VALUES (?1, ?2, 'rel', NULL, '2026-08-01T00:00:00+00:00')",
            rusqlite::params![ids.0, ids.1],
        )
        .unwrap();
    }

    // M2 при открытии (+ авто-бэкап pre-v08)
    let db = Database::new(&db_path).expect("migrate to v2");
    assert_eq!(
        db.get_kv("schema_version").unwrap().unwrap(),
        ob2h::db::schema::SCHEMA_VERSION.to_string()
    );

    let has_backups = tmp
        .path()
        .join("backups")
        .read_dir()
        .map(|d| d.filter_map(|e| e.ok()).any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("pre-v08-")
        }))
        .unwrap_or(false);
    assert!(has_backups, "миграция M2 должна создать бэкап pre-v08-*");

    // Новые колонки работают, updated_at рёбер бэкуfilled из created_at
    db.with_conn(|conn| {
        let edge_ts: String = conn
            .query_row("SELECT updated_at FROM graph_edges", [], |r| r.get(0))
            .unwrap();
        assert!(!edge_ts.is_empty(), "updated_at ребра должен быть бэкуfilled");
        Ok(())
    })
    .unwrap();

    // Даунгрейт-безопасность: INSERT в стиле v0.7.1 (именованные старые колонки)
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, created_at, updated_at)
             VALUES ('downgrade', 'строка старым кодом', 'general', 0.5, '2026-08-02T00:00:00+00:00', '2026-08-02T00:00:00+00:00')",
            [],
        )?;
        Ok(())
    })
    .unwrap();
}

/// Полный roundtrip: факты и граф, сохранённые на «pc», находятся на «vps».
#[tokio::test]
async fn test_roundtrip_pc_to_vps() {
    let dir_pc = tempdir().expect("tempdir pc");
    let dir_vps = tempdir().expect("tempdir vps");
    let pc = make_ctx(dir_pc.path(), "pc", &["pc", "vps"]);
    let vps = make_ctx(dir_vps.path(), "vps", &["pc", "vps"]);

    pc.memory
        .save("Пользователь ведёт проект ob2h на Rust", Some("proj"), "work", 0.9, "chat", None)
        .await
        .unwrap();
    pc.memory
        .save("Любимый напиток — кофе без сахара", None, "prefs", 0.8, "chat", None)
        .await
        .unwrap();

    let extraction = ExtractionResult {
        entities: vec![
            Entity { label: "Анна".into(), entity_type: "Person".into(), description: "архитектор".into() },
            Entity { label: "ob2h".into(), entity_type: "Artifact".into(), description: "память агента".into() },
        ],
        relations: vec![Relation {
            source: "Анна".into(),
            target: "ob2h".into(),
            label: "created".into(),
            contexts: vec!["Анна создала ob2h".into()],
        }],
        chunks_processed: 1,
        chunks_skipped: 0,
    };
    pc.graph.upsert_extraction(&extraction).await.unwrap();

    let bundle = pc.sync.export("vps").unwrap();
    assert!(bundle.exists());

    let stats = vps.sync.import_file(&bundle).await.unwrap();
    assert_eq!(stats.memories_applied, 2);
    assert_eq!(stats.nodes_applied, 2);
    assert_eq!(stats.edges_applied, 1);

    // Память нашлась на vps (включая векторный поиск — embedding переехал в бандле)
    let got = vps.memory.get("proj").unwrap().expect("факт доехал");
    assert!(got.content.contains("Rust"));
    let hits = vps
        .memory
        .search_hybrid("кофе", 3, 0.0)
        .await
        .unwrap();
    assert!(!hits.is_empty(), "гибридный поиск по синхронизированному факту");

    // Граф нашёлся на vps
    let found = vps.graph.search("Анна", 5, true).await.unwrap();
    assert!(!found.nodes.is_empty());
    assert!(!found.edges.is_empty());

    // Watermark: повторный экспорт без изменений — пустой бандл
    let bundle2 = pc.sync.export("vps").unwrap();
    let stats2 = vps.sync.import_file(&bundle2).await.unwrap();
    assert_eq!(stats2.memories_applied + stats2.nodes_applied + stats2.edges_applied, 0);
}

/// LWW: новее updated_at побеждает; при равенстве — приоритет origin.
#[tokio::test]
async fn test_lww_conflict_resolution() {
    let dir_pc = tempdir().expect("tempdir pc");
    let dir_vps = tempdir().expect("tempdir vps");
    let pc = make_ctx(dir_pc.path(), "pc", &["pc", "vps"]); // pc выше приоритетом
    let vps = make_ctx(dir_vps.path(), "vps", &["pc", "vps"]);

    // Обе стороны пишут один ключ с разным содержимым
    pc.memory.save("версия с ПК", Some("k"), "g", 0.5, "chat", None).await.unwrap();
    vps.memory.save("версия с VPS", Some("k"), "g", 0.5, "chat", None).await.unwrap();

    // 1. VPS новее — входящий с ПК проигрывает
    set_mem_ts(&pc, "k", "2026-08-23T10:00:00+00:00", "pc");
    set_mem_ts(&vps, "k", "2026-08-23T11:00:00+00:00", "vps");
    let b1 = pc.sync.export("vps").unwrap();
    let s1 = vps.sync.import_file(&b1).await.unwrap();
    assert_eq!(s1.conflicts_lost, 1);
    assert_eq!(vps.memory.get("k").unwrap().unwrap().content, "версия с VPS");

    // 2. ПК стал новее — входящий побеждает
    set_mem_ts(&pc, "k", "2026-08-23T12:00:00+00:00", "pc");
    let b2 = pc.sync.export("vps").unwrap();
    let s2 = vps.sync.import_file(&b2).await.unwrap();
    assert_eq!(s2.memories_applied, 1);
    assert_eq!(vps.memory.get("k").unwrap().unwrap().content, "версия с ПК");

    // 3. Равные updated_at — tie-break по приоритету origin (pc > vps)
    set_mem_ts(&vps, "k", "2026-08-23T12:00:00+00:00", "vps");
    vps.memory.save("снова с VPS", Some("k"), "g", 0.5, "chat", None).await.unwrap();
    // save сбросил origin='' и updated_at=now; выставляем контролируемо:
    set_mem_ts(&vps, "k", "2026-08-23T12:00:00+00:00", "vps");
    let b3 = pc.sync.export("vps").unwrap(); // у pc updated_at 12:00, origin pc
    let s3 = vps.sync.import_file(&b3).await.unwrap();
    assert_eq!(s3.memories_applied, 1, "при равенстве timestamp побеждает приоритет pc");
    assert_eq!(vps.memory.get("k").unwrap().unwrap().content, "версия с ПК");
}

/// Tombstone: forget на ПК реплицируется на VPS; повторный save воскрешает.
#[tokio::test]
async fn test_tombstone_replication() {
    let dir_pc = tempdir().expect("tempdir pc");
    let dir_vps = tempdir().expect("tempdir vps");
    let pc = make_ctx(dir_pc.path(), "pc", &[]);
    let vps = make_ctx(dir_vps.path(), "vps", &[]);

    pc.memory.save("временный факт", Some("tmp"), "g", 0.5, "chat", None).await.unwrap();
    let b1 = pc.sync.export("vps").unwrap();
    vps.sync.import_file(&b1).await.unwrap();
    assert!(vps.memory.get("tmp").unwrap().is_some());

    // forget = tombstone
    assert!(pc.memory.forget("tmp").unwrap());
    assert!(pc.memory.get("tmp").unwrap().is_none(), "pc: tombstone скрыт из get");

    let b2 = pc.sync.export("vps").unwrap();
    let s2 = vps.sync.import_file(&b2).await.unwrap();
    assert_eq!(s2.memories_applied, 1, "tombstone должен быть применён");
    assert!(vps.memory.get("tmp").unwrap().is_none(), "vps: факт скрыт после репликации");

    // Строка физически на месте (ждёт maintenance-чистки)
    vps.db.with_conn(|conn| {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE key='tmp' AND deleted_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        Ok(())
    })
    .unwrap();

    // Повторный save на vps снимает tombstone и делает строку «своей»
    vps.memory.save("возрождённый факт", Some("tmp"), "g", 0.6, "chat", None).await.unwrap();
    let revived = vps.memory.get("tmp").unwrap().expect("воскрешён");
    assert!(revived.content.contains("возрождённый"));
}

/// Повторный импорт того же бандла — no-op.
#[tokio::test]
async fn test_idempotent_import() {
    let dir_pc = tempdir().expect("tempdir pc");
    let dir_vps = tempdir().expect("tempdir vps");
    let pc = make_ctx(dir_pc.path(), "pc", &[]);
    let vps = make_ctx(dir_vps.path(), "vps", &[]);

    pc.memory.save("факт", Some("f1"), "g", 0.5, "chat", None).await.unwrap();
    let bundle = pc.sync.export("vps").unwrap();

    let s1 = vps.sync.import_file(&bundle).await.unwrap();
    assert!(!s1.already_applied);
    let s2 = vps.sync.import_file(&bundle).await.unwrap();
    assert!(s2.already_applied, "повторный импорт того же bundle_id — no-op");
    assert_eq!(s2.memories_applied, 0);
}

/// Битые бандлы: ошибка, БД не тронута.
#[tokio::test]
async fn test_broken_bundle() {
    let dir = tempdir().expect("tempdir");
    let vps = make_ctx(dir.path(), "vps", &[]);
    vps.memory.save("свой факт", Some("own"), "g", 0.5, "chat", None).await.unwrap();

    // Мусор вместо gzip
    let junk = dir.path().join("junk.jsonl.gz");
    std::fs::write(&junk, b"not a gzip at all").unwrap();
    assert!(vps.sync.import_file(&junk).await.is_err());

    // Валидный gzip с битой строкой внутри
    let bad = dir.path().join("bad.jsonl.gz");
    let enc = flate2::write::GzEncoder::new(std::fs::File::create(&bad).unwrap(), flate2::Compression::default());
    let mut w = std::io::BufWriter::new(enc);
    writeln!(w, r#"{{"type":"bundle","bundle_id":"x-1","origin":"pc"}}"#).unwrap();
    writeln!(w, "{{not valid json").unwrap();
    use std::io::Write as _;
    w.flush().unwrap();
    drop(w);
    assert!(vps.sync.import_file(&bad).await.is_err(), "битая строка бандла должна давать ошибку");

    // БД не изменилась
    assert!(vps.memory.get("own").unwrap().is_some());
    let s = vps.sync.status();
    assert!(s.contains("origin: vps"));
}

/// after_dream-фаза не активна без конфига: run_scheduled — тихий no-op.
#[tokio::test]
async fn test_run_scheduled_noop_without_config() {
    let dir = tempdir().expect("tempdir");
    let ctx = make_ctx(dir.path(), "local", &[]);
    ctx.sync.run_scheduled().await.expect("no-op без peers/after_dream");
}
