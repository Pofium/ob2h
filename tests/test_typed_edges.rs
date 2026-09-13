//! Ф32 (PLAN_v1.4): typed edges в dream-ревизии — вердикты 23.2 становятся рёбрами
//! (`contradicts`/`supersedes`), conflict-разметка в выдаче, soft-delete рёбер (M7),
//! belief-derivation lite за флагом `OB2H_DREAM_BELIEF` (32.3).
//! Без сети: FakeLLM (триггеры по подстрокам промптов), FakeEmbedding, in-memory БД.

use std::sync::Arc;

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::dream::Dream;
use ob2h::embedding::FakeEmbedding;
use ob2h::init_app;
use ob2h::llm::FakeLLM;
use ob2h::mcp::McpServer;
use ob2h::memory::MemoryService;
use ob2h::workspace::{GitStore, Workspace};
use rusqlite::params;
use serde_json::json;
use tempfile::tempdir;

struct Env {
    dream: Dream,
    memory: Arc<MemoryService>,
    db: Database,
    llm: Arc<FakeLLM>,
    ws: Arc<Workspace>,
}

fn setup(belief: bool) -> (tempfile::TempDir, Env) {
    let tmp = tempdir().expect("tempdir");
    let ws = Arc::new(Workspace::new(tmp.path().join("workspace")));
    let git = Arc::new(GitStore::new(tmp.path().join("workspace")));
    let db = Database::in_memory().expect("db");
    let memory = Arc::new(MemoryService::new(db.clone(), Arc::new(FakeEmbedding::new(384))));
    let llm = Arc::new(FakeLLM::new());
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.dream_extract_enabled = false;
    settings.dream_memory_revision = true; // Ф32.1: ревизия включена явно
    settings.dream_belief = belief;
    let dream = Dream::new(
        ws.clone(),
        git,
        llm.clone(),
        settings,
        db.clone(),
        None,
        Some(memory.clone()),
        None,
    );
    (
        tmp,
        Env {
            dream,
            memory,
            db,
            llm,
            ws,
        },
    )
}

async fn seed_pair(env: &Env) {
    // «Старая» запись (попадёт в ревизию по минимальному доверию) и «новая» —
    // кандидат related_key. Категории не важны: FakeEmbedding даёт разные векторы,
    // автосвязи категории не мешают вердиктам.
    env.memory
        .save(
            "Старый факт: бэкенд сайта работает на PHP 5 без очередей",
            Some("t-old"),
            "facts",
            0.3,
            "chat",
            None,
        )
        .await
        .expect("save old");
    env.memory
        .save(
            "Новый факт: бэкенд переехал на Laravel 12 с очередями",
            Some("t-new"),
            "facts",
            0.6,
            "chat",
            None,
        )
        .await
        .expect("save new");
}

fn edge_count(env: &Env, kind: &str, from_key: &str, to_key: &str) -> i64 {
    env.db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM memory_links WHERE kind = ?1 AND deleted_at IS NULL \
                 AND from_id = (SELECT id FROM memories WHERE key = ?2) \
                 AND to_id = (SELECT id FROM memories WHERE key = ?3)",
                params![kind, from_key, to_key],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .expect("edge count")
}

/// 32.1: вердикт `contradicted` c related_key → ребро kind=contradicts (от новой к старой);
/// trust старой записи падает на дельту 23.2.
#[tokio::test]
async fn contradicted_verdict_creates_contradicts_edge() {
    let (_tmp, env) = setup(false);
    seed_pair(&env).await;
    env.ws.append_history("Пользователь обсуждал переезд бэкенда").expect("history");
    env.llm.set_response(
        "минимальным доверием",
        r#"[{"key": "t-old", "verdict": "contradicted", "related_key": "t-new"}]"#,
    );
    env.llm.set_response("причинно-следственная связь", "[]");

    let stats = env.dream.run("test").await.expect("dream");
    assert_eq!(stats.status, "ok");
    assert_eq!(edge_count(&env, "contradicts", "t-new", "t-old"), 1, "{stats:?}");

    let old_trust: f64 = env
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT trust FROM memories WHERE key = 't-old'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .expect("trust");
    assert!(old_trust < 0.1, "trust старой записи падает (0.5 - 0.5): {old_trust}");
}

/// 32.1: вердикт `outdated` → ребро kind=supersedes (новая supersede старую).
#[tokio::test]
async fn outdated_verdict_creates_supersedes_edge() {
    let (_tmp, env) = setup(false);
    seed_pair(&env).await;
    env.ws.append_history("Пользователь сообщил свежую информацию").expect("history");
    env.llm.set_response(
        "минимальным доверием",
        r#"[{"key": "t-old", "verdict": "outdated", "related_key": "t-new"}]"#,
    );
    env.llm.set_response("причинно-следственная связь", "[]");

    let stats = env.dream.run("test").await.expect("dream");
    assert_eq!(stats.status, "ok");
    assert_eq!(edge_count(&env, "supersedes", "t-new", "t-old"), 1, "{stats:?}");
}

/// Тесты 32.1: повторный дрим не дублирует рёбра (upsert идемпотентен).
#[tokio::test]
async fn re_dream_does_not_duplicate_edges() {
    let (_tmp, env) = setup(false);
    seed_pair(&env).await;
    env.ws.append_history("Пользователь спорил о стеке").expect("history");
    env.llm.set_response(
        "минимальным доверием",
        r#"[{"key": "t-old", "verdict": "contradicted", "related_key": "t-new"}]"#,
    );
    env.llm.set_response("причинно-следственная связь", "[]");

    env.dream.run("test-1").await.expect("dream 1");
    env.dream.run("test-2").await.expect("dream 2");
    assert_eq!(edge_count(&env, "contradicts", "t-new", "t-old"), 1, "без дублей");
}

/// 32.3: флаг OB2H_DREAM_BELIEF=0 (дефолт) — предложения `causes` только в отчёте.
#[tokio::test]
async fn belief_flag_off_creates_no_causes_edges() {
    let (_tmp, env) = setup(false);
    seed_pair(&env).await;
    env.ws.append_history("Пользователь объяснил причину переезда").expect("history");
    env.llm.set_response("минимальным доверием", "[]");
    env.llm.set_response(
        "причинно-следственная связь",
        r#"[{"from_key": "t-old", "to_key": "t-new", "why": "тест"}]"#,
    );

    let stats = env.dream.run("test").await.expect("dream");
    assert_eq!(edge_count(&env, "causes", "t-old", "t-new"), 0, "флаг off — рёбер нет");

    // предложение попало в дрим-отчёт (memory_revision)
    let revisions = stats.memory_revision.expect("memory_revision в stats");
    assert!(
        revisions
            .iter()
            .any(|e| e.get("belief").is_some()
                && e.get("status").and_then(|s| s.as_str()).unwrap_or("").contains("не создано")),
        "предложение должно быть в отчёте: {revisions:?}"
    );
}

/// 32.3: флаг OB2H_DREAM_BELIEF=1 — предложение создаёт ребро kind=causes.
#[tokio::test]
async fn belief_flag_on_creates_causes_edges() {
    let (_tmp, env) = setup(true);
    seed_pair(&env).await;
    env.ws.append_history("Пользователь объяснил причину переезда").expect("history");
    env.llm.set_response("минимальным доверием", "[]");
    env.llm.set_response(
        "причинно-следственная связь",
        r#"[{"from_key": "t-old", "to_key": "t-new", "why": "тест"}]"#,
    );

    let stats = env.dream.run("test").await.expect("dream");
    assert_eq!(edge_count(&env, "causes", "t-old", "t-new"), 1, "{stats:?}");
}

/// 32.4: forget стороны ребра → soft-delete ребра (deleted_at), 1-hop его не видит.
#[tokio::test]
async fn forget_soft_deletes_links() {
    let (_tmp, env) = setup(false);
    seed_pair(&env).await;
    let old_id = env.memory.get("t-old").expect("get").expect("rec").id;
    let new_id = env.memory.get("t-new").expect("get").expect("rec").id;
    env.memory.upsert_link(new_id, old_id, "contradicts", 1.0).expect("link");

    env.memory.forget("t-new").expect("forget");

    let deleted: Option<String> = env
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT deleted_at FROM memory_links WHERE from_id = ?1 AND to_id = ?2",
                params![new_id, old_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .expect("deleted_at");
    assert!(deleted.is_some(), "ребро должно получить tombstone (M7)");

    // 1-hop со стороны t-old больше не видит t-new
    let related: Vec<String> = env
        .memory
        .related_records(&[old_id], 5)
        .expect("related")
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert!(!related.contains(&"t-new".to_string()), "soft-deleted ребро в 1-hop: {related:?}");
}

/// 32.2: conflict-разметка в memory_search — обе стороны спора с trust.
#[tokio::test]
async fn conflict_markup_shows_both_sides() {
    let tmp = tempdir().expect("tempdir");
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();
    let ctx = init_app(settings).expect("init app");
    let db = ctx.db.clone();
    let memory = ctx.memory.clone();
    let server = McpServer::new(ctx);

    for (key, content) in [
        ("c-a", "Кофе повышает продуктивность: да"),
        ("c-b", "Кофе повышает продуктивность: нет"),
    ] {
        memory
            .save(content, Some(key), "conflict-test", 0.5, "chat", None)
            .await
            .expect("save");
    }
    let a = memory.get("c-a").expect("get").expect("rec");
    let b = memory.get("c-b").expect("get").expect("rec");
    memory.upsert_link(b.id, a.id, "contradicts", 1.0).expect("link");

    // сервисный уровень: пара найдена среди хитов, trust обеих сторон присутствует
    let conflicts = memory.conflicts_among(&[a.id, b.id]).expect("conflicts");
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");

    let out = server
        .call_tool("memory_search", json!({"query": "кофе продуктивность", "limit": 5}))
        .await;
    assert!(out.contains("[conflict]"), "нет conflict-разметки: {out}");
    assert!(out.contains("c-a") && out.contains("c-b"), "обе стороны спора: {out}");
    assert!(out.contains("trust"), "trust сторон показан: {out}");
}
