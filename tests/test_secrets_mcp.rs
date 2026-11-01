//! Интеграционные тесты MCP-инструментов secrets_* (Фаза 58).

use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::McpServer;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn test_secrets_mcp_flow() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let mut settings = Settings::default();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();

    let ctx = init_app(settings)?;
    let server = McpServer::new(ctx.clone());

    // 1. secrets_status до init
    let res = server.call_tool("secrets_status", json!({})).await;
    let st: serde_json::Value = serde_json::from_str(&res)?;
    assert_eq!(st["initialized"], false);
    assert_eq!(st["locked"], true);

    // 2. secrets_get и secrets_set до init возвращают понятную ошибку
    let res = server.call_tool("secrets_get", json!({ "name": "api_key" })).await;
    assert!(res.contains("[Error] secrets not initialized"));

    let res = server.call_tool("secrets_set", json!({ "name": "api_key", "value": "val" })).await;
    assert!(res.contains("[Error] secrets not initialized"));

    // 3. Инициализируем хранилище через SecretsService
    ctx.secrets.init("1234", None)?;

    // 4. Попытка get/set после init, но до unlock -> [Error] secrets locked
    let res = server.call_tool("secrets_get", json!({ "name": "api_key" })).await;
    assert!(res.contains("[Error] secrets locked"));

    let res = server.call_tool("secrets_set", json!({ "name": "api_key", "value": "secret123" })).await;
    assert!(res.contains("[Error] secrets locked"));

    // 5. Разблокируем сейф (имитация ввода PIN пользователем)
    ctx.secrets.unlock("1234")?;

    // 6. Проверяем secrets_status в разблокированном виде
    let res = server.call_tool("secrets_status", json!({})).await;
    let st: serde_json::Value = serde_json::from_str(&res)?;
    assert_eq!(st["initialized"], true);
    assert_eq!(st["locked"], false);
    assert!(st["ttl_left_sec"].as_u64().unwrap() > 0);

    // 7. Сохраняем секрет через secrets_set
    let res = server.call_tool(
        "secrets_set",
        json!({
            "name": "prod_db_pass",
            "value": "super_secret_db_password_42",
            "hint": "PostgreSQL production password"
        }),
    ).await;
    assert!(res.contains("✓"));

    // 8. Читаем секрет через secrets_get
    let res = server.call_tool("secrets_get", json!({ "name": "prod_db_pass" })).await;
    assert_eq!(res, "super_secret_db_password_42");

    // 9. Блокируем через secrets_lock
    let res = server.call_tool("secrets_lock", json!({})).await;
    assert!(res.contains("✓ Хранилище секретов заблокировано"));

    // 10. После блокировки get снова заперт
    let res = server.call_tool("secrets_get", json!({ "name": "prod_db_pass" })).await;
    assert!(res.contains("[Error] secrets locked"));

    // 11. Повторный unlock и удаление
    ctx.secrets.unlock("1234")?;
    let res = server.call_tool("secrets_delete", json!({ "name": "prod_db_pass" })).await;
    assert!(res.contains("✓"));

    let res = server.call_tool("secrets_get", json!({ "name": "prod_db_pass" })).await;
    assert!(res.contains("[Error] секрет \"prod_db_pass\" не найден"));

    Ok(())
}
