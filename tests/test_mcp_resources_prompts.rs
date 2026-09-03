//! Интеграционный тест MCP Resources и MCP Prompts (Фаза 19).

use tempfile::tempdir;
use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::protocol::JsonRpcRequest;
use ob2h::mcp::McpServer;

#[tokio::test]
async fn test_mcp_resources_and_prompts() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.ensure_dirs()?;

    let ctx = init_app(settings)?;
    let server = McpServer::new(ctx.clone());

    // 0. Регистрация тестового проекта и узлов в БД
    let proj = ctx.project.register_project(
        "res_proj",
        "Resource Project",
        tmp.path().to_str().unwrap(),
        Some("Тестовый проект для проверки ресурсов"),
        Some(&["rust".to_string()]),
    )?;

    // Устанавливаем проект как активный в сессии
    *ctx.active_project_id.write().await = Some(proj.id.clone());

    ctx.db.with_conn(|conn| {
        conn.execute(
            r#"
            INSERT INTO graph_nodes (
                node_id, label, node_type, file_path, line_start, line_end, description,
                project_id, provenance, is_god_node, created_at, updated_at
            ) VALUES 
            ('fn_core', 'core_dispatcher', 'Function', 'src/core.rs', 1, 50, 'Главный диспетчер сообщений', 'res_proj', 'ast', 1, '2026-01-01', '2026-01-01'),
            ('table_users', 'users', 'Table', 'schema.sql', 1, 20, 'id INTEGER, name TEXT', 'res_proj', 'ast', 0, '2026-01-01', '2026-01-01')
            "#,
            [],
        )?;
        Ok(())
    })?;

    std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"res_proj\"\nversion = \"0.1.0\"\n")?;

    // 1. Проверка capabilities в initialize
    let uri = format!("file:///{}", tmp.path().display()).replace('\\', "/");
    let init_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(1)),
        method: "initialize".to_string(),
        params: Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "rootUri": uri
        })),
    };
    let init_resp = server.handle_request(init_req).await.expect("init response");
    let init_result = init_resp.result.expect("init result");
    assert!(init_result.get("capabilities").unwrap().get("resources").is_some());
    assert!(init_result.get("capabilities").unwrap().get("prompts").is_some());

    // 2. Тестируем resources/list
    let res_list_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(2)),
        method: "resources/list".to_string(),
        params: None,
    };
    let res_list_resp = server.handle_request(res_list_req).await.expect("res_list response");
    let resources = res_list_resp.result.unwrap().get("resources").unwrap().as_array().unwrap().clone();
    assert_eq!(resources.len(), 4);

    let uris: Vec<&str> = resources.iter().map(|r| r.get("uri").unwrap().as_str().unwrap()).collect();
    assert!(uris.contains(&"project://current/overview"));
    assert!(uris.contains(&"project://current/god-nodes"));
    assert!(uris.contains(&"project://current/schema"));
    assert!(uris.contains(&"memory://context"));

    // 3. Тестируем resources/read
    let read_schema_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(3)),
        method: "resources/read".to_string(),
        params: Some(serde_json::json!({ "uri": "project://current/schema" })),
    };
    let read_schema_resp = server.handle_request(read_schema_req).await.expect("read_schema response");
    let schema_text = read_schema_resp.result.unwrap().get("contents").unwrap()[0].get("text").unwrap().as_str().unwrap().to_string();
    assert!(schema_text.contains("users"));

    let read_god_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(4)),
        method: "resources/read".to_string(),
        params: Some(serde_json::json!({ "uri": "project://current/god-nodes" })),
    };
    let read_god_resp = server.handle_request(read_god_req).await.expect("read_god response");
    let god_text = read_god_resp.result.unwrap().get("contents").unwrap()[0].get("text").unwrap().as_str().unwrap().to_string();
    assert!(god_text.contains("core_dispatcher"));

    // 4. Тестируем prompts/list
    let prompt_list_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(5)),
        method: "prompts/list".to_string(),
        params: None,
    };
    let prompt_list_resp = server.handle_request(prompt_list_req).await.expect("prompt_list response");
    let prompts = prompt_list_resp.result.unwrap().get("prompts").unwrap().as_array().unwrap().clone();
    assert_eq!(prompts.len(), 2);
    let prompt_names: Vec<&str> = prompts.iter().map(|p| p.get("name").unwrap().as_str().unwrap()).collect();
    assert!(prompt_names.contains(&"explain_component"));
    assert!(prompt_names.contains(&"plan_feature"));

    // 5. Тестируем prompts/get для explain_component
    let prompt_get_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(6)),
        method: "prompts/get".to_string(),
        params: Some(serde_json::json!({
            "name": "explain_component",
            "arguments": { "component_name": "core_dispatcher" }
        })),
    };
    let prompt_get_resp = server.handle_request(prompt_get_req).await.expect("prompt_get response");
    let prompt_content = prompt_get_resp.result.unwrap();
    let user_msg = prompt_content.get("messages").unwrap()[0].get("content").unwrap().get("text").unwrap().as_str().unwrap().to_string();
    assert!(user_msg.contains("core_dispatcher"));
    assert!(user_msg.contains("Главный диспетчер сообщений"));

    // 6. Тестируем prompts/get для plan_feature
    let plan_get_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(7)),
        method: "prompts/get".to_string(),
        params: Some(serde_json::json!({
            "name": "plan_feature",
            "arguments": { "task_description": "Добавить кэширование Redis" }
        })),
    };
    let plan_get_resp = server.handle_request(plan_get_req).await.expect("plan_get response");
    let plan_content = plan_get_resp.result.unwrap();
    let plan_msg = plan_content.get("messages").unwrap()[0].get("content").unwrap().get("text").unwrap().as_str().unwrap().to_string();
    assert!(plan_msg.contains("Добавить кэширование Redis"));

    Ok(())
}
