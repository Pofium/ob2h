//! Ф34 (PLAN_v1.4): Sync v2 — дельта-экспорт по курсору, `--full`, поле-уровневый
//! merge с журналом sync/conflicts.jsonl и keep-losers, memory_links/trust в бандле,
//! `sync verify` (детект дрейфа). Без сети: два SyncManager в tempdir, in-memory БД.

use std::collections::HashMap;
use std::io::Read as _;
use std::sync::Arc;

use flate2::read::GzDecoder;
use ob2h::backup::BackupManager;
use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::sync::SyncManager;
use rusqlite::params;
use serde_json::{json, Value};
use tempfile::tempdir;

/// env-зависимые тесты (OB2H_SYNC_KEEP_LOSERS) сериализуем мьютексом —
/// переменная процесса глобальна, параллельные тесты не должны мешать друг другу.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Node {
    sync: SyncManager,
    db: Database,
    settings: Settings,
    _tmp: tempfile::TempDir,
}

fn node(origin: &str) -> Node {
    let tmp = tempdir().expect("tempdir");
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    let db = Database::in_memory().expect("db");
    let embedder: Arc<FakeEmbedding> = Arc::new(FakeEmbedding::new(384));
    let backup = BackupManager::new(settings.clone(), db.clone());
    let _sync = SyncManager::new(
        settings.clone(),
        db.clone(),
        embedder.clone() as Arc<dyn ob2h::embedding::EmbeddingProvider>,
        Arc::new(backup),
    );
    // идентичность узла — через peers.json (origin/priority)
    let sync_dir = settings.data_dir.join("sync");
    std::fs::create_dir_all(&sync_dir).expect("sync dir");
    std::fs::write(
        sync_dir.join("peers.json"),
        json!({ "origin": origin, "priority": [origin, "other"] }).to_string(),
    )
    .expect("peers.json");
    // перечитываем конфиг новым менеджером (NodeConfig::load на new)
    let sync2 = SyncManager::new(
        settings.clone(),
        db.clone(),
        embedder.clone() as Arc<dyn ob2h::embedding::EmbeddingProvider>,
        Arc::new(BackupManager::new(settings.clone(), db.clone())),
    );
    Node {
        sync: sync2,
        db,
        settings,
        _tmp: tmp,
    }
}

async fn save(node: &Node, key: &str, content: &str, importance: f64) {
    use ob2h::memory::MemoryService;
    let memory = MemoryService::new(node.db.clone(), Arc::new(FakeEmbedding::new(384)));
    memory
        .save(content, Some(key), "facts", importance, "chat", None)
        .await
        .expect("save");
}

fn sql_rows(node: &Node, sql: &str) -> Vec<(String, Option<String>)> {
    node.db
        .with_conn(|conn| {
            let mut stmt = conn.prepare(sql)?;
            let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
            Ok(it.flatten().collect())
        })
        .expect("sql")
}

fn bundle_text(path: &std::path::Path) -> String {
    let raw = std::fs::read(path).expect("bundle read");
    let mut s = String::new();
    GzDecoder::new(&raw[..])
        .read_to_string(&mut s)
        .expect("gunzip");
    s
}

fn bundle_lines(text: &str) -> (Value, Vec<Value>) {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Value = serde_json::from_str(lines.next().expect("header")).expect("header json");
    let rows = lines
        .map(|l| serde_json::from_str::<Value>(l).expect("row json"))
        .collect();
    (header, rows)
}

/// 34.1: дельта — после N правок второй бандл несёт ровно N строк mem.
#[tokio::test]
async fn delta_transfers_exactly_changed_rows() {
    let n = node("pc");
    save(&n, "s-1", "первая запись", 0.5).await;
    save(&n, "s-2", "вторая запись", 0.5).await;
    // уводим базовый батч в прошлое: таймстемпы записей — секунды, watermark = max_ts;
    // чтобы неизменённая s-2 не попадала в дельту (`>=` граница), её ts строго меньше
    n.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET updated_at = '2026-09-13T00:00:00Z' WHERE key IN ('s-1','s-2')",
                [],
            )?;
            Ok(())
        })
        .expect("backdate");

    let b1 = n.sync.export("vps").expect("export 1");
    let (_, rows1) = bundle_lines(&bundle_text(&b1));
    let mem1 = rows1.iter().filter(|r| r["type"] == "mem").count();
    assert!(mem1 >= 2, "первый бандл — всё содержимое: {mem1}");

    // N=3 правки/добавления
    save(&n, "s-3", "третья запись", 0.5).await;
    save(&n, "s-4", "четвёртая запись", 0.5).await;
    n.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET content = 'обновлённая первая', updated_at = '2099-01-01T00:00:00+00:00' \
                 WHERE key = 's-1'",
                [],
            )?;
            Ok(())
        })
        .expect("update");

    let b2 = n.sync.export("vps").expect("export 2");
    let (_, rows2) = bundle_lines(&bundle_text(&b2));
    let mem2: Vec<&Value> = rows2.iter().filter(|r| r["type"] == "mem").collect();
    assert_eq!(mem2.len(), 3, "дельта несёт ровно N строк: {mem2:?}");
    let keys: Vec<&str> = mem2
        .iter()
        .filter_map(|r| r["key"].as_str())
        .collect();
    assert!(keys.contains(&"s-3") && keys.contains(&"s-4") && keys.contains(&"s-1"));
}

/// 34.3: v2-бандл возит trust/last_feedback_at и memory_links; round-trip PC→VPS→PC
/// без потерь (34.5-тест round-trip).
#[tokio::test]
async fn round_trip_preserves_trust_and_links() {
    let pc = node("pc");
    let vps = node("vps");
    save(&pc, "r-a", "запись с доверием", 0.6).await;
    save(&pc, "r-b", "связанная запись", 0.5).await;
    pc.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET trust = 0.9, last_feedback_at = '2099-01-01T00:00:00+00:00' \
                 WHERE key = 'r-a'",
                [],
            )?;
            Ok(())
        })
        .expect("trust");
    let (a_id, b_id): (i64, i64) = pc
        .db
        .with_conn(|conn| {
            let a: i64 = conn.query_row("SELECT id FROM memories WHERE key='r-a'", [], |r| r.get(0))?;
            let b: i64 = conn.query_row("SELECT id FROM memories WHERE key='r-b'", [], |r| r.get(0))?;
            Ok((a, b))
        })
        .expect("ids");
    pc.db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_links (from_id, to_id, kind, weight, created_at) \
                 VALUES (?1, ?2, 'manual', 1.0, '2099-01-01T00:00:00+00:00')",
                params![a_id, b_id],
            )?;
            Ok(())
        })
        .expect("link");

    let bundle = pc.sync.export("vps").expect("export pc");
    let stats = vps.sync.import_file(&bundle).await.expect("import vps");
    assert!(stats.memories_applied >= 2 && stats.links_applied >= 1, "{stats:?}");

    // VPS получил trust/last_feedback_at/ребро
    let (trust, fb): (f64, Option<String>) = vps
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT trust, last_feedback_at FROM memories WHERE key = 'r-a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )})
        .expect("trust on vps");
    assert!((trust - 0.9).abs() < 1e-9, "trust доехал: {trust}");
    assert_eq!(fb.as_deref(), Some("2099-01-01T00:00:00+00:00"));
    let links = sql_rows(
        &vps,
        "SELECT fk.key, l.deleted_at FROM memory_links l \
         JOIN memories fk ON fk.id = l.from_id WHERE l.deleted_at IS NULL",
    );
    assert!(links.iter().any(|(k, _)| k == "r-a"), "ребро доехало живым");

    // VPS правит контент (новее) и экспортирует обратно — PC получает правку,
    // trust не теряется, ребро живо (round-trip)
    vps.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET content = 'правка с VPS', updated_at = '2099-06-01T00:00:00+00:00' \
                 WHERE key = 'r-a'",
                [],
            )?;
            Ok(())
        })
        .expect("vps edit");
    let bundle2 = vps.sync.export("pc").expect("export vps");
    let stats2 = pc.sync.import_file(&bundle2).await.expect("import pc");
    assert!(stats2.memories_applied >= 1, "{stats2:?}");

    let (content, trust_pc): (String, f64) = pc
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT content, trust FROM memories WHERE key = 'r-a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )})
        .expect("pc state");
    assert_eq!(content, "правка с VPS");
    assert!((trust_pc - 0.9).abs() < 1e-9, "trust не потерян: {trust_pc}");
    let links_pc = sql_rows(
        &pc,
        "SELECT fk.key, l.deleted_at FROM memory_links l \
         JOIN memories fk ON fk.id = l.from_id WHERE l.deleted_at IS NULL",
    );
    assert!(links_pc.iter().any(|(k, d)| k == "r-a" && d.is_none()));
}

/// 34.2: конфликт content при OB2H_SYNC_KEEP_LOSERS=1 — проигравшая версия
/// в meta.conflict_versions, запись в sync/conflicts.jsonl.
// MutexGuard держится через await намеренно: тест сериализует env-переменную
// против параллельных импортов других тестов.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn keep_losers_saves_conflict_version_and_journal() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("OB2H_SYNC_KEEP_LOSERS", "1");

    let pc = node("pc");
    save(&pc, "k-x", "версия PC", 0.5).await;
    // входящая строка «новее» — LWW отдаёт ей content, PC-версия в meta
    let bundle_path = pc.settings.data_dir.join("in").join("b.jsonl.gz");
    std::fs::create_dir_all(bundle_path.parent().unwrap()).expect("in dir");
    write_bundle(
        &bundle_path,
        "remote-node",
        Some(2),
        json!({"type": "mem", "key": "k-x", "content": "версия VPS", "category": "facts",
               "importance": 0.5, "source": "chat", "meta": "{\"note\":\"vps\"}",
               "created_at": "2099-02-01T00:00:00+00:00",
               "updated_at": "2099-02-01T00:00:00+00:00",
               "origin": "vps", "deleted_at": null, "project_id": null,
               "trust": 0.5, "last_feedback_at": null}),
    );
    let stats = pc.sync.import_file(&bundle_path).await.expect("import");
    assert_eq!(stats.conflicts_journaled, 1, "{stats:?}");

    let (content, meta): (String, Option<String>) = pc
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT content, meta FROM memories WHERE key = 'k-x'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )})
        .expect("row");
    assert_eq!(content, "версия VPS", "LWW: входящая новее");
    let meta: Value = serde_json::from_str(meta.as_deref().unwrap_or("{}")).unwrap_or(json!({}));
    assert_eq!(meta["note"], "vps", "meta — union");
    let cv = meta["conflict_versions"].as_array().expect("conflict_versions");
    assert!(
        cv.iter().any(|v| v["content"] == "версия PC" && v["origin"] == "pc"),
        "проигравшая версия сохранена (StateFuse): {meta}"
    );

    let journal = std::fs::read_to_string(pc.settings.data_dir.join("sync/conflicts.jsonl"))
        .expect("conflicts.jsonl");
    assert!(journal.contains("k-x") && journal.contains("версия PC"), "{journal}");

    std::env::remove_var("OB2H_SYNC_KEEP_LOSERS");
    drop(_guard);
}

/// 34.2: v1-бандл (без version) читается — старая семантика: meta заменяется целиком.
#[tokio::test]
async fn v1_bundle_still_imports() {
    let pc = node("pc");
    save(&pc, "v-1", "локальная запись", 0.5).await;
    pc.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET meta = '{\"local\":1}', updated_at = '2088-01-01T00:00:00+00:00' \
                 WHERE key = 'v-1'",
                [],
            )?;
            Ok(())
        })
        .expect("meta");
    let bundle_path = pc.settings.data_dir.join("in").join("v1.jsonl.gz");
    std::fs::create_dir_all(bundle_path.parent().unwrap()).expect("in dir");
    // v1: нет поля version; строка mem без trust/last_feedback_at
    write_bundle(
        &bundle_path,
        "vps",
        None,
        json!({"type": "mem", "key": "v-1", "content": "правка из v1", "category": "facts",
               "importance": 0.5, "source": "chat", "meta": "{\"remote\":1}",
               "created_at": "2089-01-01T00:00:00+00:00",
               "updated_at": "2089-01-01T00:00:00+00:00",
               "origin": "vps", "deleted_at": null, "project_id": null}),
    );
    let stats = pc.sync.import_file(&bundle_path).await.expect("import v1");
    assert_eq!(stats.memories_applied, 1, "{stats:?}");
    let meta: Option<String> = pc
        .db
        .with_conn(|conn| {
            conn.query_row("SELECT meta FROM memories WHERE key = 'v-1'", [], |r| r.get(0))})
        .expect("meta");
    let meta: Value = serde_json::from_str(meta.as_deref().unwrap_or("{}")).unwrap_or(json!({}));
    assert_eq!(meta["remote"], 1, "v1: входящая meta целиком");
    assert!(meta.get("local").is_none(), "v1: без union (старая семантика)");
}

/// 34.4: verify детектит искусственный дрейф (сравнение статистик без ssh).
#[test]
fn verify_compare_detects_drift() {
    let same = json!({
        "memories_total": 2, "memories_alive": 2, "trust_avg": 0.5,
        "links_alive": 1, "links_deleted": 0,
        "mem_checksum": "aaaa", "links_checksum": "bbbb"
    });
    let report = SyncManager::compare_stats(&same, &same);
    assert!(report.contains("согласованы"), "{report}");
    assert!(!report.contains("ДРЕЙФ"), "{report}");

    let mut drifted = same.clone();
    drifted["mem_checksum"] = json!("deadbeef");
    drifted["links_alive"] = json!(2);
    let report = SyncManager::compare_stats(&same, &drifted);
    assert!(report.contains("обнаружен дрейф"), "{report}");
    assert!(report.matches("ДРЕЙФ").count() >= 2, "{report}");
}

/// 34.3 (ADR-K6): ralph-таблицы в бандл не попадают.
#[tokio::test]
async fn ralph_tables_are_not_exported() {
    let n = node("pc");
    // проект + ralph_run: если экспорт трогал ralph-таблицы, они бы появились в бандле
    n.db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name, root_path, created_at, updated_at) \
                 VALUES ('p-ralph', 'SyncProj', '/tmp/sync-proj', '2099-01-01T00:00:00+00:00', '2099-01-01T00:00:00+00:00')",
                [],
            )?;
            conn.execute(
                "INSERT INTO ralph_runs (id, project_id, feature_slug, goal, status, autonomy, \
                 max_iterations_per_task, max_total_iterations, created_at, updated_at) \
                 VALUES ('run-1', 'p-ralph', 'feat', 'ralphgoal-marker-xyz', 'running', 'L1', 5, 60, \
                 '2099-01-01T00:00:00+00:00', '2099-01-01T00:00:00+00:00')",
                [],
            )?;
            Ok(())
        })
        .expect("ralph seed");

    let bundle = n.sync.export("vps").expect("export");
    let text = bundle_text(&bundle);
    assert!(
        !text.contains("ralphgoal-marker-xyz") && !text.contains("run-1"),
        "ralph-таблицы в бандле (ADR-K6)"
    );
    assert!(!text.contains("ast_change"), "ast_changes в бандле (ADR-K6)");
}

/// Вспомогательное: написать бандл вручную (одна mem-строка); version=None → v1.
fn write_bundle(path: &std::path::Path, origin: &str, version: Option<u8>, row: Value) {
    
    use std::io::Write as _;
    let file = std::fs::File::create(path).expect("bundle file");
    let mut w = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut header = json!({
        "type": "bundle",
        "bundle_id": format!("{origin}-manual"),
        "origin": origin,
        "peer": "manual",
        "created_at": "2099-01-01T00:00:00+00:00",
        "from": "",
        "to": "2099-01-01T00:00:00+00:00",
        "counts": {"mem": 1, "node": 0, "edge": 0, "mlink": 0},
    });
    if let Some(v) = version {
        header["version"] = json!(v);
    }
    writeln!(w, "{header}").expect("header");
    writeln!(w, "{row}").expect("row");
    w.finish().expect("gzip finish");
}

/// Заглушка для clippy: HashMap нужен для сигнатур sync, здесь не используется.
#[allow(dead_code)]
fn _unused(_m: &HashMap<String, String>) {}
