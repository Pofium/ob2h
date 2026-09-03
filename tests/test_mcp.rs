use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::McpServer;
use ob2h::mcp::tools::list_tools;
use tempfile::tempdir;

/// Снапшот контракта: 24 инструмента (19 базовых + 5 проектных v1.0).
#[test]
fn test_tools_list_contract_snapshot() {
    let names: Vec<String> = list_tools().into_iter().map(|t| t.name).collect();
    let expected = [
        "memory_save",
        "memory_search",
        "memory_update",
        "memory_forget",
        "memory_context",
        "workspace_read",
        "workspace_write",
        "session_log",
        "knowledge_extract",
        "graph_search",
        "graph_reason",
        "graph_stats",
        "dream_run",
        "dream_status",
        "dream_log",
        "dream_restore",
        "omnes_stats",
        "omnes_backup",
        "session_ingest",
        // v1.0: Проектные инструменты и AST-граф
        "project_init",
        "project_scan",
        "project_context",
        "project_graph_search",
        "project_report",
        // v1.2: Анализ радиуса изменений (Blast Radius)
        "project_impact",
    ];
    assert_eq!(names, expected, "контракт tools/list изменился — см. PLAN_v1.2.md");
}

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
                "importance": 0.95,
                "project_id": "ob2h"
            }),
        )
        .await;
    assert!(save_out.starts_with("saved key="));

    // 2. memory_search
    let search_out = server
        .call_tool(
            "memory_search",
            serde_json::json!({ "query": "Hermes ob2h", "project_id": "ob2h" }),
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

    // 8. project_init
    let p_init_out = server
        .call_tool(
            "project_init",
            serde_json::json!({
                "id": "my_proj",
                "name": "My Project",
                "path": tmp.path().to_str().unwrap(),
                "description": "Test project via MCP",
                "tech_stack": ["rust", "mcp"]
            }),
        )
        .await;
    assert!(p_init_out.contains("project registered: id=my_proj"));

    // 9. project_scan
    let p_scan_out = server
        .call_tool(
            "project_scan",
            serde_json::json!({ "id": "my_proj" }),
        )
        .await;
    assert!(p_scan_out.contains("project 'my_proj' scanned:"));

    // 10. project_context
    let p_ctx_out = server
        .call_tool(
            "project_context",
            serde_json::json!({ "id": "my_proj" }),
        )
        .await;
    assert!(p_ctx_out.contains("<project_context id=\"my_proj\">"));

    // 11. project_report
    let p_rep_out = server
        .call_tool(
            "project_report",
            serde_json::json!({ "id": "my_proj" }),
        )
        .await;
    assert!(p_rep_out.contains("Архитектурный дайджест проекта: My Project"));
}

#[tokio::test]
async fn test_session_ingest_pairs_and_dedup() {
    let tmp = tempdir().expect("tempdir");
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();

    let ctx = init_app(settings).expect("init app");
    let server = McpServer::new(ctx);

    let mk = |role: &str, content: &str| serde_json::json!({ "role": role, "content": content });

    // 1. Первый вызов: две пары (два user подряд склеиваются в одну пару)
    let out1 = server
        .call_tool(
            "session_ingest",
            serde_json::json!({
                "messages": [
                    mk("user", "Привет"),
                    mk("user", "Как дела?"),
                    mk("assistant", "Привет! Отлично."),
                    mk("tool", "tool call — должен быть пропущен"),
                    mk("assistant", "Ответ без user-сообщения")
                ],
                "session_id": "sess-1"
            }),
        )
        .await;
    assert!(out1.starts_with("ingested pairs=2"), "got: {out1}");

    // 2. Повтор той же транскрипты — дедуп, ничего нового
    let out2 = server
        .call_tool(
            "session_ingest",
            serde_json::json!({
                "messages": [
                    mk("user", "Привет"),
                    mk("user", "Как дела?"),
                    mk("assistant", "Привет! Отлично."),
                    mk("tool", "tool call — должен быть пропущен"),
                    mk("assistant", "Ответ без user-сообщения")
                ],
                "session_id": "sess-1"
            }),
        )
        .await;
    assert!(out2.starts_with("ingested pairs=0"), "got: {out2}");
    assert!(out2.contains("skipped_msgs=5"), "got: {out2}");

    // 3. Тот же session_id, транскрипта выросла — пишется только хвост (1 новая пара)
    let out3 = server
        .call_tool(
            "session_ingest",
            serde_json::json!({
                "messages": [
                    mk("user", "Привет"),
                    mk("user", "Как дела?"),
                    mk("assistant", "Привет! Отлично."),
                    mk("tool", "tool call — должен быть пропущен"),
                    mk("assistant", "Ответ без user-сообщения"),
                    mk("user", "Расскажи о памяти"),
                    mk("assistant", "Память — это ob2h.")
                ],
                "session_id": "sess-1"
            }),
        )
        .await;
    assert!(out3.starts_with("ingested pairs=1"), "got: {out3}");
    assert!(out3.contains("skipped_msgs=5"), "got: {out3}");

    // 4. Daily-лог содержит все три пары (2 + 1), с session_id в meta
    let daily_dir = tmp.path().join("workspace").join("daily");
    let mut total_lines = 0usize;
    for entry in std::fs::read_dir(&daily_dir).expect("daily dir") {
        let path = entry.expect("entry").path();
        let content = std::fs::read_to_string(&path).expect("daily file");
        total_lines += content.lines().filter(|l| !l.trim().is_empty()).count();
    }
    assert_eq!(total_lines, 3, "в daily-логе должно быть ровно 3 пары");

    // 5. Без session_id дедупа нет — та же пара пишется снова
    let out5 = server
        .call_tool(
            "session_ingest",
            serde_json::json!({
                "messages": [mk("user", "q"), mk("assistant", "a")]
            }),
        )
        .await;
    assert!(out5.starts_with("ingested pairs=1"), "got: {out5}");

    // 6. Пустой/отсутствующий messages — ошибка контракта
    let out6 = server
        .call_tool("session_ingest", serde_json::json!({ "messages": [] }))
        .await;
    assert!(out6.starts_with("[Error]"), "got: {out6}");
}
