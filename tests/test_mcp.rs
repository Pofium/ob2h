use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::McpServer;
use tempfile::tempdir;

#[tokio::test]
async fn test_mcp_all_tools_dispatch() {
    let tmp = tempdir().expect("tempdir");
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();

    let ctx = init_app(settings).expect("init app");
    let server = McpServer::new(ctx);

    // 1. memory_save
    let save_out = server
        .call_tool(
            "memory_save",
            serde_json::json!({
                "content": "Hermes использует локальный MCP сервер ob2h",
                "category": "system",
                "importance": 0.95
            }),
        )
        .await;
    assert!(save_out.starts_with("saved key="));

    // 2. memory_search
    let search_out = server
        .call_tool(
            "memory_search",
            serde_json::json!({ "query": "Hermes ob2h" }),
        )
        .await;
    assert!(search_out.contains("Hermes"));

    // 3. workspace_read / workspace_write
    let write_out = server
        .call_tool(
            "workspace_write",
            serde_json::json!({
                "file": "user",
                "content": "Владелец системы: разработчик ИИ"
            }),
        )
        .await;
    assert!(write_out.starts_with("written user"));

    let read_out = server
        .call_tool("workspace_read", serde_json::json!({ "file": "user" }))
        .await;
    assert!(read_out.contains("разработчик ИИ"));

    // 4. session_log
    let log_out = server
        .call_tool(
            "session_log",
            serde_json::json!({
                "user_text": "Привет, как дела?",
                "assistant_text": "Привет! Всё отлично, память работает."
            }),
        )
        .await;
    assert!(log_out.starts_with("logged"));

    // 5. omnes_stats
    let stats_out = server.call_tool("omnes_stats", serde_json::json!({})).await;
    assert!(stats_out.contains("memories="));

    // 6. graph_stats
    let gstats_out = server.call_tool("graph_stats", serde_json::json!({})).await;
    assert!(gstats_out.contains("nodes="));

    // 7. dream_status
    let dream_status = server.call_tool("dream_status", serde_json::json!({})).await;
    assert!(dream_status.contains("last_run:"));
}
