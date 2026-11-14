//! Тесты хронологии и рекапа (v1.9.9, Фаза 66): окно вокруг даты/текста,
//! детерминированный recap с инкрементальным курсором (кэш-хит без LLM),
//! dream-слот-рефлекс. Без сети: FakeLLM + fake embedder + in-memory БД.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::init_app;
use ob2h::llm::{FakeLLM, LLMClient};
use ob2h::lifecycle::timeline::{load_entries, recap_session, render_timeline, resolve_anchor, window};
use tempfile::tempdir;

struct CountingLLM {
    inner: Arc<FakeLLM>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMClient for CountingLLM {
    async fn ask(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.ask(prompt, system).await
    }
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
    let mut content = String::new();
    content.push_str(&entry(
        "2026-11-13T10:00:00Z",
        "починить вход по JWT",
        "исправил refresh-токены",
        "sess_a",
    ));
    content.push('\n');
    content.push_str(&entry(
        "2026-11-13T11:30:00Z",
        "TODO: переписать миграции M12",
        "запланировал",
        "sess_a",
    ));
    content.push('\n');
    std::fs::write(daily.join("2026-11-13.jsonl"), &content).unwrap();
    let mut content2 = String::new();
    content2.push_str(&entry(
        "2026-11-14T09:00:00Z",
        "настройка кеша редис",
        "поднял redis, error rate упал",
        "sess_b",
    ));
    content2.push('\n');
    content2.push_str(&entry(
        "2026-11-14T15:00:00Z",
        "вечерний рефакторинг",
        "закрыл PR",
        "sess_c",
    ));
    content2.push('\n');
    std::fs::write(daily.join("2026-11-14.jsonl"), content2).unwrap();
    ws
}

#[test]
fn test_timeline_window_around_date_and_text() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let entries = load_entries(&ws)?;
    assert_eq!(entries.len(), 4);

    // Якорь-дата: первая запись 2026-11-14 (индекс 2), окно 1 до / 1 после
    let idx = resolve_anchor(&entries, "2026-11-14").expect("якорь-дата");
    assert_eq!(entries[idx].timestamp, "2026-11-14T09:00:00Z");
    let win = window(&entries, Some(idx), 1, 1);
    assert_eq!(win.len(), 3);
    assert!(win[0].timestamp.starts_with("2026-11-13T11:30"));
    assert!(win[2].timestamp.starts_with("2026-11-14T15:00"));

    // Якорь-текст: «JWT» — первая запись
    let idx = resolve_anchor(&entries, "JWT").expect("якорь-текст");
    assert!(entries[idx].user_text.contains("JWT"));

    // Без якоря — хвост (последние before записей)
    let win = window(&entries, None, 2, 0);
    assert_eq!(win.len(), 2);
    assert!(win[1].timestamp.starts_with("2026-11-14T15:00"));

    // Рендер несёт сессию/автора и обрезку
    let md = render_timeline(&win);
    assert!(md.contains("sess_c"), "{md}");
    assert!(md.contains("tester"));
    assert!(md.contains("- user:"));
    Ok(())
}

fn make_ctx(dir: &std::path::Path) -> Arc<ob2h::mcp::AppContext> {
    let mut settings = Settings::from_env();
    settings.data_dir = dir.to_path_buf();
    settings.embed_provider = "fake".to_string();
    init_app(settings).expect("init_app")
}

#[tokio::test]
async fn test_recap_incremental_cursor_and_cache_hit() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let db = Database::in_memory()?;
    let fake = Arc::new(FakeLLM::new());
    fake.set_default_response("- исправлен вход по JWT\n- TODO: миграции M12");
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = Arc::new(CountingLLM {
        inner: fake,
        calls: calls.clone(),
    });

    // Первый рекап sess_a: LLM-вызов, digest фиксирован
    let out = recap_session(&ws, &db, llm.as_ref(), None, Some("sess_a"), false).await?;
    assert_eq!(out["cached"], false);
    assert_eq!(out["pairs_total"], 2);
    assert_eq!(out["digest"], "- исправлен вход по JWT\n- TODO: миграции M12");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Повторный рекап неизменённой сессии: кэш-хит, LLM-вызова НЕТ (66.3)
    let out2 = recap_session(&ws, &db, llm.as_ref(), None, Some("sess_a"), false).await?;
    assert_eq!(out2["cached"], true);
    assert_eq!(out2["digest"], out["digest"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "кэш-хит не должен звать LLM");

    // Сессия выросла: только хвост идёт в LLM, курсор сдвигается
    let daily = ws.daily_dir().join("2026-11-13.jsonl");
    let mut text = std::fs::read_to_string(&daily)?;
    let extra = serde_json::json!({
        "timestamp": "2026-11-13T12:00:00Z",
        "user_text": "добавили тесты",
        "assistant_text": "зелёные",
        "meta": {"source": "hermes", "session_id": "sess_a"}
    })
    .to_string()
        + "\n";
    text.push_str(&extra);
    std::fs::write(&daily, text)?;
    let out3 = recap_session(&ws, &db, llm.as_ref(), None, Some("sess_a"), false).await?;
    assert_eq!(out3["cached"], false);
    assert_eq!(out3["pairs_total"], 3);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "LLM зовётся только на хвост");
    Ok(())
}

#[tokio::test]
async fn test_recap_save_to_memory_with_provenance() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let ctx = make_ctx(tmp.path());
    let fake = Arc::new(FakeLLM::new());
    fake.set_default_response("- итог тестовой сессии");
    ctx.llm.swap(fake);

    let out = ob2h::lifecycle::timeline::recap_session(
        &ws,
        &ctx.db,
        ctx.llm.as_ref(),
        Some(ctx.memory.as_ref()),
        Some("sess_a"),
        true,
    )
    .await?;
    let key = out["saved_key"].as_str().expect("saved_key");
    assert!(out["digest"].as_str().unwrap().contains("итог тестовой"));

    // Факт в memories: category/source = recap, meta несёт session_id и source_ids
    let (category, source, meta): (String, String, String) = ctx.db.with_conn(|c| {
        c.query_row(
            "SELECT category, source, meta FROM memories WHERE key = ?1",
            rusqlite::params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default())),
        )
    })?;
    assert_eq!(category, "recap");
    assert_eq!(source, "recap");
    let m: serde_json::Value = serde_json::from_str(&meta)?;
    assert_eq!(m["session_id"], "sess_a");
    let ids = m["source_ids"].as_array().expect("source_ids");
    assert!(ids.iter().any(|v| v.as_str().unwrap().contains("daily/2026-11-13.jsonl#L")));
    Ok(())
}

#[tokio::test]
async fn test_recap_without_llm_extractive_fallback() -> anyhow::Result<()> {
    // LLM, который всегда ошибается → extractive-фолбэк (детерминированный)
    struct BrokenLLM;
    #[async_trait]
    impl LLMClient for BrokenLLM {
        async fn ask(&self, _p: &str, _s: Option<&str>) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("offline"))
        }
    }
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let db = Database::in_memory()?;
    let out = recap_session(&ws, &db, &BrokenLLM, None, Some("sess_a"), false).await?;
    let digest = out["digest"].as_str().unwrap();
    assert!(digest.contains("Пользователь"), "{digest}");
    assert!(digest.contains("починить вход по JWT"));
    Ok(())
}

#[tokio::test]
async fn test_dream_reflect_slots() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ws = ws_with_logs(tmp.path());
    let db = Database::in_memory()?;

    // Лог текущего дня с TODO (ws_with_logs писал фиксированные даты 2026-11 —
    // dream_reflect смотрит «сегодня», поэтому пишем сегодняшний файл)
    let today = ob2h::db::utcnow()[..10].to_string();
    let daily = ws.daily_dir().join(format!("{today}.jsonl"));
    std::fs::write(
        &daily,
        serde_json::json!({
            "timestamp": ob2h::db::utcnow(),
            "user_text": "TODO: обновить документацию синка",
            "assistant_text": "пометил; error в тесте 42 поправлен",
            "meta": {"source": "hermes", "session_id": "sess_d"}
        })
        .to_string()
        + "\n",
    )?;

    let report = ob2h::lifecycle::slots::dream_reflect(&ws, &db)?;
    assert_eq!(report["todos_found"], 1);
    assert_eq!(report["todos_appended"], 1);
    assert_eq!(report["guidance_updated"], true);

    // Слоты в глобальном скоупе (у дрима нет проектного контекста)
    let svc = ob2h::lifecycle::SlotsService::new(db.clone());
    let pending = svc.get("pending_items", None)?;
    assert!(pending.content.contains("TODO: обновить документацию синка"));
    assert_eq!(pending.scope, "global");
    let guidance = svc.get("session_guidance", None)?;
    assert!(
        guidance.content.contains("пар диалога сегодня: 1"),
        "guidance: {} | report: {report}",
        guidance.content
    );

    // Повторный рефлекс: дубликата TODO нет
    let report2 = ob2h::lifecycle::slots::dream_reflect(&ws, &db)?;
    assert_eq!(report2["todos_appended"], 0);
    let pending2 = svc.get("pending_items", None)?;
    assert_eq!(
        pending2.content.matches("TODO: обновить документацию синка").count(),
        1
    );
    Ok(())
}
