//! Ф31 (PLAN_v1.4): офлайн-консолидация — LLM-вердикты (31.2), compaction (31.3),
//! дедуп-отчёт CLI (31.5). Без сети: FakeLLM + FakeEmbedding + in-memory БД.

use std::sync::Arc;

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::dream::consolidate;
use ob2h::dream::Dream;
use ob2h::embedding::FakeEmbedding;
use ob2h::llm::FakeLLM;
use ob2h::memory::MemoryService;
use ob2h::workspace::{GitStore, Workspace};
use rusqlite::params;
use tempfile::tempdir;

struct Env {
    dream: Dream,
    memory: Arc<MemoryService>,
    db: Database,
    llm: Arc<FakeLLM>,
}

fn setup() -> (tempfile::TempDir, Env) {
    let tmp = tempdir().expect("tempdir");
    let ws = Arc::new(Workspace::new(tmp.path().join("workspace")));
    let git = Arc::new(GitStore::new(tmp.path().join("workspace")));
    let db = Database::in_memory().expect("db");
    let embedder = Arc::new(FakeEmbedding::new(384));
    let memory = Arc::new(MemoryService::new(db.clone(), embedder));
    let llm = Arc::new(FakeLLM::new());
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.dream_extract_enabled = false;
    settings.dream_memory_revision = false; // изолируем консолидацию от ревизии trust
    let dream = Dream::new(
        ws,
        git,
        llm.clone(),
        settings,
        db.clone(),
        None,
        Some(memory.clone()),
    );
    (tmp, Env { dream, memory, db, llm })
}

fn set_trust(db: &Database, key: &str, trust: f64) {
    db.with_conn(|conn| {
        conn.execute("UPDATE memories SET trust = ?1 WHERE key = ?2", params![trust, key])?;
        Ok(())
    })
    .expect("trust");
}

fn set_meta(db: &Database, key: &str, meta: &str) {
    db.with_conn(|conn| {
        conn.execute("UPDATE memories SET meta = ?1 WHERE key = ?2", params![meta, key])?;
        Ok(())
    })
    .expect("meta");
}

fn row(db: &Database, sql: &str, key: &str) -> Option<serde_json::Value> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(params![key])?;
        if let Some(r) = rows.next()? {
            let deleted_at: Option<String> = r.get(1).unwrap_or(None);
            let meta: String = r.get(2).unwrap_or_default();
            return Ok(Some(serde_json::json!({
                "deleted": deleted_at.is_some(),
                "meta": meta,
            })));
        }
        Ok(None)
    })
    .expect("row")
}

const SELECT_MEM: &str =
    "SELECT key, deleted_at, meta FROM memories WHERE key = ?1";

async fn save(env: &Env, key: &str, content: &str, importance: f64) {
    env.memory
        .save(content, Some(key), "notes", importance, "chat", None)
        .await
        .expect("save");
}

// --- 31.2: merge ------------------------------------------------------------

#[tokio::test]
async fn merge_absorbs_updates_canonical_and_redirects_links() {
    let (_tmp, env) = setup();
    save(&env, "k-a", "Пользователь работает в компании Яндекс", 0.5).await;
    save(&env, "k-b", "Пользователь работает в Яндексе", 0.8).await;
    save(&env, "k-c", "Совершенно другая запись про кота", 0.5).await;
    set_trust(&env.db, "k-a", 0.9);
    set_trust(&env.db, "k-b", 0.4);

    // маркер 31.1: k-a подозревает дублирование с k-b
    set_meta(&env.db, "k-a", r#"{"merge_candidate": "k-b"}"#);

    // ссылка на k-b — после слияния должна указывать на k-a
    env.db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_links (from_id, to_id, kind, weight, created_at) \
                 SELECT (SELECT id FROM memories WHERE key='k-c'), id, 'manual', 1.0, '2026-09-13' \
                 FROM memories WHERE key = 'k-b'",
                [],
            )?;
            Ok(())
        })
        .expect("link");

    env.llm.set_response(
        "Консолидация памяти",
        r#"{"verdict": "merge", "canonical_key": "k-a", "note": "одно и то же знание"}"#,
    );

    let report = env.dream.consolidate_memory().await.expect("report");
    assert_eq!(report["merges"].as_array().unwrap().len(), 1);
    assert_eq!(report["merges"][0]["canonical"], "k-a");
    assert_eq!(report["merges"][0]["absorbed"], "k-b");

    // каноническая: max importance (0.8), sum access, маркер снят
    let a = row(&env.db, SELECT_MEM, "k-a").expect("k-a");
    assert!(!a["deleted"].as_bool().unwrap());
    let a_meta: serde_json::Value = serde_json::from_str(a["meta"].as_str().unwrap()).unwrap();
    assert!(a_meta.get("merge_candidate").is_none());

    // поглощённая: tombstone + merged_into, маркер снят
    let b = row(&env.db, SELECT_MEM, "k-b").expect("k-b");
    assert!(b["deleted"].as_bool().unwrap(), "поглощённая запись — tombstone");
    let b_meta: serde_json::Value = serde_json::from_str(b["meta"].as_str().unwrap()).unwrap();
    assert_eq!(b_meta["merged_into"], "k-a");
    assert!(b_meta.get("merge_candidate").is_none());

    // поглощённая вне search
    assert!(env.memory.get("k-b").expect("get").is_none());

    // links перенаправлены: k-c → k-a (старая строка k-c → k-b удалена)
    let redirected: i64 = env
        .db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM memory_links l \
                 JOIN memories f ON f.id = l.from_id JOIN memories t ON t.id = l.to_id \
                 WHERE f.key = 'k-c' AND t.key = 'k-a' AND l.kind = 'manual'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("links");
    assert_eq!(redirected, 1, "ссылка перепривязана к канонической");
    let stale: i64 = env
        .db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM memory_links l JOIN memories t ON t.id = l.to_id \
                 WHERE t.key = 'k-b'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("links");
    assert_eq!(stale, 0, "ссылок на поглощённую запись не осталось");
}

#[tokio::test]
async fn supersedes_keeps_both_alive_with_edge() {
    let (_tmp, env) = setup();
    save(&env, "k-old", "База данных проекта: Postgres", 0.5).await;
    save(&env, "k-new", "База данных проекта: MySQL", 0.5).await;
    // k-new свежее — ребро пойдёт от него
    env.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET updated_at = '2026-09-01T00:00:00+00:00' WHERE key = 'k-old'",
                [],
            )?;
            Ok(())
        })
        .expect("ts");
    set_meta(&env.db, "k-old", r#"{"merge_candidate": "k-new"}"#);

    env.llm.set_response(
        "Консолидация памяти",
        r#"{"verdict": "supersedes", "note": "смена состояния"}"#,
    );

    let report = env.dream.consolidate_memory().await.expect("report");
    assert_eq!(report["edges"][0]["kind"], "supersedes");
    assert_eq!(report["edges"][0]["from"], "k-new");
    assert_eq!(report["edges"][0]["to"], "k-old");

    // обе записи живы, история не теряется
    assert!(!row(&env.db, SELECT_MEM, "k-old").unwrap()["deleted"].as_bool().unwrap());
    assert!(!row(&env.db, SELECT_MEM, "k-new").unwrap()["deleted"].as_bool().unwrap());

    // ребро зарегистрировано, маркеры сняты
    let edges: i64 = env
        .db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM memory_links WHERE kind = 'supersedes'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("edges");
    assert_eq!(edges, 1);
}

#[tokio::test]
async fn contradicts_creates_edge_once_on_rerun() {
    let (_tmp, env) = setup();
    save(&env, "k-1", "Деплой только через systemd", 0.5).await;
    save(&env, "k-2", "Деплой только через Docker", 0.5).await;
    set_meta(&env.db, "k-1", r#"{"merge_candidate": "k-2"}"#);

    env.llm.set_response(
        "Консолидация памяти",
        r#"{"verdict": "contradicts", "note": "взаимоисключающие правила"}"#,
    );

    let _ = env.dream.consolidate_memory().await.expect("report 1");
    let _ = env.dream.consolidate_memory().await.expect("report 2");

    // повторный дрим не дублирует рёбра
    let edges: i64 = env
        .db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM memory_links WHERE kind = 'contradicts'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("edges");
    assert_eq!(edges, 1);
}

#[tokio::test]
async fn keep_both_changes_nothing_but_clears_marker() {
    let (_tmp, env) = setup();
    save(&env, "k-x", "Любимый цвет: синий", 0.5).await;
    save(&env, "k-y", "Любимый напиток: чай", 0.5).await;
    set_meta(&env.db, "k-x", r#"{"merge_candidate": "k-y"}"#);

    env.llm.set_response(
        "Консолидация памяти",
        r#"{"verdict": "keep_both", "note": "разные знания"}"#,
    );

    let report = env.dream.consolidate_memory().await.expect("report");
    assert_eq!(report["keep_both"].as_array().unwrap().len(), 1);
    assert_eq!(report["edges"].as_array().unwrap().len(), 0);
    assert!(!row(&env.db, SELECT_MEM, "k-x").unwrap()["deleted"].as_bool().unwrap());
    assert!(!row(&env.db, SELECT_MEM, "k-y").unwrap()["deleted"].as_bool().unwrap());

    // маркер снят — группа не будет разбираться повторно
    let x = row(&env.db, SELECT_MEM, "k-x").unwrap();
    let x_meta: serde_json::Value = serde_json::from_str(x["meta"].as_str().unwrap()).unwrap();
    assert!(x_meta.get("merge_candidate").is_none());
}

// --- 31.3: compaction --------------------------------------------------------

#[tokio::test]
async fn compaction_clusters_low_trust_skips_high_trust_and_throttles() {
    let (_tmp, env) = setup();
    // три слабые записи со схожими ключами (Jaccard ≥ 0.5 → один кластер)
    save(&env, "hmem-old-fact-a", "Заметка про старый проект альфа", 0.2).await;
    save(&env, "hmem-old-fact-b", "Заметка про старый проект бета", 0.2).await;
    save(&env, "hmem-old-fact-c", "Заметка про старый проект гамма", 0.2).await;
    for k in ["hmem-old-fact-a", "hmem-old-fact-b", "hmem-old-fact-c"] {
        set_trust(&env.db, k, 0.2);
    }
    // high-trust запись с похожим ключом — в кластер не попадает
    save(&env, "hmem-old-fact-z", "Важное подтверждённое правило", 0.2).await;
    set_trust(&env.db, "hmem-old-fact-z", 0.9);

    env.llm.set_response(
        "Сжатие памяти",
        "Дайджест: старые заметки проекта (альфа/бета/гамма) объединены.",
    );

    let report = env.dream.consolidate_memory().await.expect("report");
    let digests = report["digests"].as_array().unwrap();
    assert_eq!(digests.len(), 1, "один кластер из трёх слабых записей");

    let digest_key = digests[0].as_str().unwrap();
    assert!(digest_key.starts_with("hmem-digest/"));

    // дайджест создан, оригиналы НЕ тронуты (дополнение к candidate_for_forget)
    let digest_id: i64 = env
        .db
        .with_conn(|conn| {
            Ok(conn.query_row("SELECT id FROM memories WHERE key = ?1", params![digest_key], |r| {
                r.get(0)
            })?)
        })
        .expect("digest");
    for k in ["hmem-old-fact-a", "hmem-old-fact-b", "hmem-old-fact-c", "hmem-old-fact-z"] {
        assert!(!row(&env.db, SELECT_MEM, k).unwrap()["deleted"].as_bool().unwrap(), "{k} жив");
    }

    // summary-рёбра: дайджест → три члена; high-trust не связан
    let linked: i64 = env
        .db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM memory_links WHERE kind = 'summary' AND from_id = ?1",
                params![digest_id],
                |r| r.get(0),
            )?)
        })
        .expect("summary links");
    assert_eq!(linked, 3);

    // троттлинг 30 дней: повторный прогон не создаёт дайджестов
    let report2 = env.dream.consolidate_memory().await.expect("report 2");
    assert_eq!(report2["digests"].as_array().unwrap().len(), 0);
}

// --- 31.5: дедуп-ядро CLI (границы 0.98/0.75) --------------------------------

#[test]
fn dedup_core_boundaries_and_canonical_choice() {
    let db = Database::in_memory().expect("db");
    let v = |x: &[f32]| ob2h::vector::similarity::serialize(x);
    db.with_conn(|conn| {
        // k-ident: пары с cos 1.0 (identity-дубль, порог 0.98)
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, source, meta, embedding, created_at, updated_at) \
             VALUES ('k-ident', 'тот же факт', 'n', 0.5, 'chat', '{}', ?1, '2026-09-13', '2026-09-13')",
            params![v(&[1.0, 0.0, 0.0])],
        )?;
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, source, meta, embedding, created_at, updated_at) \
             VALUES ('k-ident2', 'тот же факт', 'n', 0.5, 'chat', '{}', ?1, '2026-09-13', '2026-09-13')",
            params![v(&[1.0, 0.0, 0.0])],
        )?;
        // k-suspect пара: cos 0.85 (между 0.75 и 0.98); обе ортогональны ident-группе
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, source, meta, embedding, created_at, updated_at) \
             VALUES ('k-suspect', 'похожий факт', 'n', 0.5, 'chat', '{}', ?1, '2026-09-13', '2026-09-13')",
            params![v(&[0.0, 1.0, 0.0])],
        )?;
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, source, meta, embedding, created_at, updated_at) \
             VALUES ('k-suspect2', 'похожий факт', 'n', 0.5, 'chat', '{}', ?1, '2026-09-13', '2026-09-13')",
            params![v(&[0.0, 0.85, 0.5268])],
        )?;
        // k-low trust ниже — каноническим предлагается k-suspect (выше trust)
        conn.execute(
            "UPDATE memories SET trust = 0.9 WHERE key IN ('k-ident', 'k-suspect')",
            [],
        )?;
        conn.execute(
            "UPDATE memories SET trust = 0.3 WHERE key IN ('k-ident2', 'k-suspect2')",
            [],
        )?;
        Ok(())
    })
    .expect("seed");

    let pairs = ob2h::cli::dedup::collect_pairs(&db).expect("pairs");
    assert_eq!(pairs.len(), 2, "identity + подозрение");

    let ident = pairs.iter().find(|p| p.identity).expect("identity pair");
    assert_eq!(ident.a_key, "k-ident");
    assert_eq!(ident.b_key, "k-ident2");
    assert_eq!(ident.canonical_key, "k-ident", "канонический — выше trust");

    let suspect = pairs.iter().find(|p| !p.identity).expect("suspect pair");
    assert_eq!(suspect.canonical_key, "k-suspect");

    // маркеры ставятся на менее доверенную запись
    let marked = ob2h::cli::dedup::apply_markers(&db, &pairs).expect("markers");
    assert_eq!(marked, 2);
    let m = row(&db, SELECT_MEM, "k-ident2").unwrap();
    let meta: serde_json::Value = serde_json::from_str(m["meta"].as_str().unwrap()).unwrap();
    assert_eq!(meta["merge_candidate"], "k-ident");
}
