//! Интеграционный тест Zero-Config автодетекта проектов и сессионного контекста MCP (Фаза 16).

use std::fs;
use tempfile::tempdir;
use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::init_app;
use ob2h::mcp::protocol::JsonRpcRequest;
use ob2h::mcp::McpServer;
use ob2h::project::{detect_manifest_metadata, find_project_root, ProjectService};

#[tokio::test]
async fn test_find_project_root_and_detect_manifest() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let root = tmp.path();

    // Создаём структуру Cargo-проекта
    let cargo_toml = r#"
    [package]
    name = "my-awesome-tool"
    version = "0.1.0"
    description = "Test awesome tool for AI agents"
    "#;
    fs::write(root.join("Cargo.toml"), cargo_toml)?;

    let nested_dir = root.join("src").join("handlers").join("nested");
    fs::create_dir_all(&nested_dir)?;
    fs::write(nested_dir.join("worker.rs"), "// worker code")?;

    // 1. Проверяем подъём к корню из глубоко вложенной поддиректории
    let detected_root = find_project_root(&nested_dir);
    assert_eq!(
        std::fs::canonicalize(detected_root)?,
        std::fs::canonicalize(root)?
    );

    // 2. Проверяем извлечение метаданных манифеста
    let (name, tech, desc) = detect_manifest_metadata(root);
    assert_eq!(name, "my-awesome-tool");
    assert!(tech.contains(&"rust".to_string()));
    assert_eq!(desc, Some("Test awesome tool for AI agents".to_string()));

    // 3. Проверяем auto_register_or_detect
    let db = Database::in_memory()?;
    let project_service = ProjectService::new(db.conn_arc());

    let proj = project_service.auto_register_or_detect(&nested_dir)?;
    assert_eq!(proj.id, "my-awesome-tool");
    assert_eq!(proj.name, "my-awesome-tool");
    assert!(proj.tech_stack.unwrap_or_default().contains("rust"));

    // Повторный вызов не дублирует, а возвращает существующий
    let proj_second = project_service.auto_register_or_detect(&nested_dir)?;
    assert_eq!(proj_second.id, proj.id);

    Ok(())
}

#[tokio::test]
async fn test_mcp_initialize_and_implicit_project_id() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let root = tmp.path();

    // Создаём Node/TS проект
    let pkg_json = r#"{
        "name": "frontend-app",
        "description": "Super web UI",
        "dependencies": { "react": "^18.0.0" }
    }"#;
    fs::write(root.join("package.json"), pkg_json)?;

    let app_dir = root.join("src").join("components");
    fs::create_dir_all(&app_dir)?;

    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().join("ob2h_data");

    let ctx = init_app(settings)?;
    let server = McpServer::new(ctx.clone());

    // 1. Эмулируем вызов MCP "initialize" с указанием rootUri во вложенной папке
    let root_uri = format!("file:///{}", app_dir.to_string_lossy().replace('\\', "/"));
    let init_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(1)),
        method: "initialize".to_string(),
        params: Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "rootUri": root_uri,
            "clientInfo": { "name": "Cursor", "version": "0.45" }
        })),
    };

    let init_resp = server.handle_request(init_req).await;
    assert!(init_resp.is_some());

    // Проверяем, что активный проект в сессии установлен
    let active_id = ctx.active_project_id.read().await.clone();
    assert_eq!(active_id, Some("frontend-app".to_string()));

    // 2. Вызываем memory_save БЕЗ project_id
    let save_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(2)),
        method: "tools/call".to_string(),
        params: Some(serde_json::json!({
            "name": "memory_save",
            "arguments": {
                "content": "Используется компонентный подход React",
                "category": "architecture"
            }
        })),
    };

    let save_resp = server.handle_request(save_req).await;
    assert!(save_resp.is_some());

    // 3. Проверяем в БД, что воспоминание сохранилось с project_id = "frontend-app"
    let memory_project_id: Option<String> = ctx.db.with_conn(|conn| {
        conn.query_row(
            "SELECT project_id FROM memories WHERE content LIKE '%React%'",
            [],
            |r| r.get(0),
        )
    })?;

    assert_eq!(memory_project_id, Some("frontend-app".to_string()));

    Ok(())
}
