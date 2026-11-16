//! Тесты аудит-леджера (v1.9.9, Фаза 68): инструментирование операций, reason у
//! memory_forget, фильтр/лимит memory_audit, ротация 90 дней, секреты — только имена.

use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::McpServer;
use tempfile::tempdir;

fn make_ctx(dir: &std::path::Path) -> std::sync::Arc<ob2h::mcp::AppContext> {
    let mut settings = Settings::from_env();
    settings.data_dir = dir.to_path_buf();
    settings.embed_provider = "fake".to_string();
    init_app(settings).expect("init_app")
}

async fn audit_actions(server: &McpServer) -> Vec<String> {
    let out = server
        .call_tool("memory_audit", serde_json::json!({ "limit": 200 }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap_or("").to_string())
        .collect()
}

#[tokio::test]
async fn test_instrumented_ops_write_audit() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ctx = make_ctx(tmp.path());
    let server = McpServer::new(ctx.clone());

    // Память: save → feedback → merge → forget с reason
    let key_a = ctx
        .memory
        .save("факт A для аудита", None, "general", 0.5, "chat", None)
        .await?;
    let key_b = ctx
        .memory
        .save("факт B для аудита", None, "general", 0.5, "chat", None)
        .await?;
    let out = server
        .call_tool(
            "memory_feedback",
            serde_json::json!({ "key": key_a, "verdict": "helpful" }),
        )
        .await;
    assert!(out.starts_with("feedback recorded"), "{out}");
    let out = server
        .call_tool(
            "memory_merge",
            serde_json::json!({ "keys": [key_a, key_b], "canonical_key": key_a, "note": "дубль теста" }),
        )
        .await;
    assert!(!out.starts_with("[Error]"), "{out}");
    let key_c = ctx
        .memory
        .save("факт C на удаление", None, "general", 0.5, "chat", None)
        .await?;
    let out = server
        .call_tool(
            "memory_forget",
            serde_json::json!({ "key": key_c, "reason": "устарел по задаче 42" }),
        )
        .await;
    assert!(out.starts_with("forgotten"), "{out}");

    // Воркспейс: имя файла и размер, но не контент
    let out = server
        .call_tool(
            "workspace_write",
            serde_json::json!({ "file": "MEMORY.md", "content": "секретное содержимое файла которое не должно попасть в аудит" }),
        )
        .await;
    assert!(out.starts_with("written"), "{out}");

    let actions = audit_actions(&server).await;
    for expected in [
        "memory_feedback",
        "memory_merge",
        "memory_forget",
        "workspace_write",
    ] {
        assert!(actions.contains(&expected.to_string()), "нет {expected}: {actions:?}");
    }

    // reason дошёл в details, контент workspace_write — НЕ дошёл
    let out = server
        .call_tool("memory_audit", serde_json::json!({ "operation": "memory_forget" }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&out)?;
    assert_eq!(v["entries"][0]["details"]["reason"], "устарел по задаче 42");
    let out = server
        .call_tool("memory_audit", serde_json::json!({ "operation": "workspace_write" }))
        .await;
    assert!(!out.contains("секретное содержимое файла"), "контент утёк в аудит: {out}");
    Ok(())
}

#[tokio::test]
async fn test_secrets_audited_names_only() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ctx = make_ctx(tmp.path());
    let server = McpServer::new(ctx.clone());

    ctx.secrets.init("1234", None)?;
    ctx.secrets.unlock("1234")?;
    let out = server
        .call_tool(
            "secrets_set",
            serde_json::json!({ "name": "api.key", "value": "super-secret-value-123", "hint": "тест" }),
        )
        .await;
    assert!(out.contains("успешно зашифрован"), "{out}");
    let out = server.call_tool("secrets_get", serde_json::json!({ "name": "api.key" })).await;
    assert!(out.contains("super-secret-value-123"), "{out}");

    // Фильтр по точному action ("secrets_" как префикс не матчится) —
    // берём весь леджер целиком
    let out = server.call_tool("memory_audit", serde_json::json!({ "limit": 100 })).await;
    assert!(out.contains("\"secrets_set\""), "{out}");
    assert!(out.contains("\"secrets_get\""), "{out}");
    // Значение секрета НИКОГДА не попадает в леджер (ADR-21)
    assert!(!out.contains("super-secret-value-123"), "значение секрета утекло: {out}");
    Ok(())
}

#[tokio::test]
async fn test_audit_prune_retention() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let ctx = make_ctx(tmp.path());
    let server = McpServer::new(ctx.clone());

    // Свежая запись: реальный feedback по живому факту
    let key = ctx
        .memory
        .save("факт для ротации", None, "general", 0.5, "chat", None)
        .await?;
    server
        .call_tool("memory_feedback", serde_json::json!({ "key": key, "verdict": "helpful" }))
        .await;

    // «Старая» запись 120 дней назад — должна вычиститься ротацией (90)
    ctx.db.with_conn(|c| {
        c.execute(
            "INSERT INTO audit_log (ts, actor, action, target_ids, details) \
             VALUES ('2026-01-01T00:00:00Z', 'test', 'old_action', '[]', '{}')",
            [],
        )?;
        Ok(())
    })?;

    let ran = ob2h::lifecycle::audit::prune_weekly(&ctx.db, 90)?;
    assert!(ran.is_some(), "первый вызов должен отработать");
    let (left, old_left): (i64, i64) = ctx.db.with_conn(|c| {
        let total: i64 = c.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))?;
        let old: i64 = c.query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'old_action'",
            [],
            |r| r.get(0),
        )?;
        Ok((total, old))
    })?;
    assert_eq!(old_left, 0, "старые записи вычищены");
    assert!(left >= 1, "свежие записи ротацией не тронуты: {left}");

    // Курсор: немедленный повтор — None (неделя не прошла)
    assert!(ob2h::lifecycle::audit::prune_weekly(&ctx.db, 90)?.is_none());
    Ok(())
}

/// Issue #2.2: `memory_feedback` без key — контрактная ошибка с подсказкой,
/// где взять key (и что id уроков не подходит), а не голое «key is required».
#[tokio::test]
async fn test_memory_feedback_missing_key_contract_hint() {
    let tmp = tempdir().expect("tempdir");
    let ctx = make_ctx(&tmp.path());
    let server = McpServer::new(ctx);

    let out = server
        .call_tool(
            "memory_feedback",
            serde_json::json!({ "verdict": "helpful", "note": "полезно" }),
        )
        .await;
    assert!(out.contains("[Error] key is required"), "got: {out}");
    assert!(out.contains("memory_search"), "подсказка с источником key: {out}");
    assert!(out.contains("lsn_"), "подсказка про id уроков: {out}");
}
