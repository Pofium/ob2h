//! Тесты провенанса фактов (v1.9.9, Фаза 67): цитаты по session-мете и
//! source_ids, честный `provenance: none`, tombstone не теряет цитаты, latency.

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::init_app;
use ob2h::lifecycle::timeline::verify_memory;
use std::sync::Arc;
use tempfile::tempdir;

fn make_ctx(dir: &std::path::Path) -> Arc<ob2h::mcp::AppContext> {
    let mut settings = Settings::from_env();
    settings.data_dir = dir.to_path_buf();
    settings.embed_provider = "fake".to_string();
    init_app(settings).expect("init_app")
}

fn ws_with_logs(dir: &std::path::Path) -> ob2h::workspace::Workspace {
    let ws = ob2h::workspace::Workspace::new(dir.join("workspace"));
    let daily = ws.daily_dir();
    std::fs::create_dir_all(&daily).unwrap();
    let entry = |ts: &str, u: &str, a: &str, sid: &str| {
        serde_json::json!({
            "timestamp": ts, "user_text": u, "assistant_text": a,
            "meta": {"source": "hermes", "session_id": sid, "author": "tester"}
        })
        .to_string()
    };
    std::fs::write(
        daily.join("2026-11-14.jsonl"),
        format!(
            "{}\n{}\n",
            entry("2026-11-14T09:00:00Z", "задача про JWT", "исправил refresh", "sess_v"),
            entry("2026-11-14T10:00:00Z", "второй ход", "готово", "sess_v")
        ),
    )
    .unwrap();
    ws
}

#[tokio::test]
async fn test_verify_session_meta_citations() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let ctx = make_ctx(tmp.path());

    // Факт с сессионной метой (как их пишет плагин/дрим через meta)
    let meta = serde_json::json!({"session_id": "sess_v", "author": "tester"}).to_string();
    ctx.memory
        .save("пользователь чинил вход по JWT", None, "general", 0.5, "chat", Some(&meta))
        .await?;

    let id: i64 = ctx.db.with_conn(|c| {
        c.query_row("SELECT id FROM memories WHERE content LIKE '%JWT%'", [], |r| r.get(0))
    })?;
    let out = verify_memory(&ws, &ctx.db, &id.to_string())?;
    assert_eq!(out["provenance"], "daily");
    assert!(out["citation_count"].as_u64().unwrap() >= 2, "{out}");
    let cits = out["citations"].as_array().unwrap();
    assert!(cits.iter().any(|c| c["user"].as_str().unwrap().contains("JWT")));
    assert!(cits.iter().all(|c| c["session_id"] == "sess_v"));
    Ok(())
}

#[tokio::test]
async fn test_verify_source_ids_exact_citations() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let ctx = make_ctx(tmp.path());

    // Recap-стиль: явные source_ids — точные цитаты по строкам
    let meta = serde_json::json!({"source_ids": ["daily/2026-11-14.jsonl#L2"]}).to_string();
    let key = ctx
        .memory
        .save("рекап сессии sess_v", None, "recap", 0.6, "recap", Some(&meta))
        .await?;
    let out = verify_memory(&ws, &ctx.db, &key)?;
    assert_eq!(out["provenance"], "daily");
    let cits = out["citations"].as_array().unwrap();
    assert_eq!(cits.len(), 1);
    assert_eq!(cits[0]["ref"], "daily/2026-11-14.jsonl#L2");
    assert_eq!(cits[0]["user"], "второй ход");

    // Битая ссылка — честный missing, не ошибка
    let meta2 = serde_json::json!({"source_ids": ["daily/2026-11-14.jsonl#L99"]}).to_string();
    let key2 = ctx
        .memory
        .save("рекап с битой ссылкой", None, "recap", 0.6, "recap", Some(&meta2))
        .await?;
    let out2 = verify_memory(&ws, &ctx.db, &key2)?;
    assert_eq!(out2["citations"][0]["missing"], true);
    Ok(())
}

#[tokio::test]
async fn test_verify_no_meta_and_tombstone() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let ctx = make_ctx(tmp.path());

    // Факт без меты → provenance: none (не ошибка)
    ctx.memory.save("факт без меты", None, "general", 0.5, "chat", None).await?;
    let id: i64 = ctx.db.with_conn(|c| {
        c.query_row("SELECT id FROM memories WHERE content = 'факт без меты'", [], |r| r.get(0))
    })?;
    let out = verify_memory(&ws, &ctx.db, &id.to_string())?;
    assert_eq!(out["provenance"], "none");
    assert_eq!(out["citation_count"], 0);

    // Tombstone: цитаты не теряются
    let meta = serde_json::json!({"session_id": "sess_v"}).to_string();
    let key = ctx
        .memory
        .save("удалённый факт с сессией", None, "general", 0.5, "chat", Some(&meta))
        .await?;
    ctx.memory.forget(&key)?; // tombstone (ADR-11), строка остаётся
    let out2 = verify_memory(&ws, &ctx.db, &key)?;
    assert_eq!(out2["record"]["deleted_at"].is_null(), false);
    assert_eq!(out2["provenance"], "daily");
    assert!(out2["citation_count"].as_u64().unwrap() >= 2);

    // Неизвестная запись — ошибка
    assert!(verify_memory(&ws, &ctx.db, "nope").is_err());
    Ok(())
}

#[tokio::test]
async fn test_verify_latency_on_synthetic_corpus() -> anyhow::Result<()> {
    // DoD Ф67: замер латентности. Синтетический корпус 5000 пар daily + 100 фактов.
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let ctx = make_ctx(tmp.path());
    let daily = ws.daily_dir().join("2026-11-14.jsonl");
    let mut corpus = std::fs::read_to_string(&daily)?;
    for i in 0..5000 {
        let line = serde_json::json!({
            "timestamp": format!("2026-11-14T{:02}:{:02}:{:02}Z", i / 3600 % 24, i / 60 % 60, i % 60),
            "user_text": format!("ход номер {i} про систему"),
            "assistant_text": "ответ",
            "meta": {"source": "hermes", "session_id": format!("sess_bulk_{}", i % 100)}
        })
        .to_string()
            + "\n";
        corpus.push_str(&line);
    }
    std::fs::write(&daily, corpus)?;

    // Факт последней «сессии» (sess_bulk_99: 50 пар) — скан по session_id
    let meta = serde_json::json!({"session_id": "sess_bulk_99"}).to_string();
    ctx.memory.save("факт bulk", None, "general", 0.5, "chat", Some(&meta)).await?;

    let start = std::time::Instant::now();
    let out = verify_memory(&ws, &ctx.db, &(
        ctx.db.with_conn(|c| {
            c.query_row("SELECT id FROM memories WHERE content = 'факт bulk'", [], |r| r.get::<_, i64>(0))
        })?
        .to_string()
    ))?;
    let elapsed = start.elapsed();
    assert_eq!(out["provenance"], "daily");
    println!(
        "Ф67 latency на корпусе 5002 пар: {:?} (цитат: {})",
        elapsed,
        out["citation_count"]
    );
    // Щедрый гейт на CI-машинах; на живой БД владельца замер попадёт в отчёт фазы
    assert!(elapsed.as_millis() < 500, "verify медленнее 500 мс: {elapsed:?}");
    Ok(())
}
