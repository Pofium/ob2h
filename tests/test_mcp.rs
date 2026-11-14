use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::McpServer;
use ob2h::mcp::tools::list_tools;
use tempfile::tempdir;

/// Снапшот контракта: 56 инструментов (v1.9.9: +4 lesson_* Ф64, +6 slot_* Ф65,
/// +memory_timeline/session_recap Ф66, +memory_verify Ф67, +memory_audit Ф68,
/// +viewer_open Ф72)
/// (41 v1.9: 26 базовых v1.3 + 7 Ralph + memory_merge + project_scan_status + 5 secrets_*).
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
        "project_scan_status",
        "project_context",
        "project_graph_search",
        // v1.4 / трек C: структурные запросы (Фаза 36) — №35, вставлен после graph_search
        "project_call_path",
        "project_report",
        // v1.2: Анализ радиуса изменений (Blast Radius)
        "project_impact",
        // v1.3: Trust и feedback-петля (Фаза 23)
        "memory_feedback",
        // v1.3: Ralph Knowledge Layer (Фазы 26–27)
        "ralph_start",
        "ralph_iteration",
        "ralph_verdict",
        "ralph_context",
        "ralph_report",
        "ast_diff",
        "ast_history",
        // v1.4: Явное слияние дублей (Фаза 31)
        "memory_merge",
        // v1.9: Secrets Vault (Фаза 58)
        "secrets_status",
        "secrets_set",
        "secrets_get",
        "secrets_delete",
        "secrets_lock",
        // v1.9.9: уроки (Фаза 64)
        "lesson_save",
        "lesson_recall",
        "lesson_list",
        "lesson_delete",
        // v1.9.9: слоты (Фаза 65)
        "slot_list",
        "slot_get",
        "slot_create",
        "slot_append",
        "slot_replace",
        "slot_delete",
        // v1.9.9: хронология и рекап (Фаза 66)
        "memory_timeline",
        "session_recap",
        // v1.9.9: провенанс (Фаза 67)
        "memory_verify",
        // v1.9.9: аудит-леджер (Фаза 68)
        "memory_audit",
        // v1.9.9: окно просмотра (Фаза 72)
        "viewer_open",
    ];
    assert_eq!(names, expected, "контракт tools/list изменился — см. PLAN_v1.9.9.md");
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

    // 9. project_scan — фоновый job с ожиданием до 45 с (малый проект успевает)
    let p_scan_out = server
        .call_tool(
            "project_scan",
            serde_json::json!({ "id": "my_proj" }),
        )
        .await;
    assert!(
        p_scan_out.contains("project 'my_proj' scanned:")
            || p_scan_out.contains("still running"),
        "got: {p_scan_out}"
    );

    // 9a. project_scan_status — досматриваем результат без CLI/логов
    let mut status_out = String::new();
    for _ in 0..50 {
        status_out = server
            .call_tool("project_scan_status", serde_json::json!({ "id": "my_proj" }))
            .await;
        if !status_out.contains("job: running") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(status_out.contains("job: done"), "got: {status_out}");
    assert!(status_out.contains("embedded="), "got: {status_out}");
    assert!(
        status_out.contains("db: ast_nodes=") && status_out.contains("last_scanned="),
        "got: {status_out}"
    );
    assert!(
        !status_out.starts_with("[Error]"),
        "status не должен быть ошибкой: {status_out}"
    );

    // 9b. project_scan wait=false — мгновенный ack фоного запуска
    let p_scan_bg = server
        .call_tool(
            "project_scan",
            serde_json::json!({ "id": "my_proj", "wait": false }),
        )
        .await;
    assert!(
        p_scan_bg.contains("started in background"),
        "got: {p_scan_bg}"
    );

    // 9c. статус несуществующего проекта — [Error]
    let bad_status = server
        .call_tool("project_scan_status", serde_json::json!({ "id": "nope" }))
        .await;
    assert!(bad_status.starts_with("[Error]"), "got: {bad_status}");

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

    // 12. viewer_open (Ф72.1): headless — честная ошибка; gui — already_running
    // по живому lockfile (окно в тесте не спавним).
    let viewer_out = {
        #[cfg(feature = "gui")]
        {
            std::fs::write(tmp.path().join("viewer.lock"), std::process::id().to_string())
                .expect("lockfile");
            let out = server.call_tool("viewer_open", serde_json::json!({})).await;
            std::fs::remove_file(tmp.path().join("viewer.lock")).ok();
            out
        }
        #[cfg(not(feature = "gui"))]
        {
            server.call_tool("viewer_open", serde_json::json!({})).await
        }
    };
    #[cfg(feature = "gui")]
    assert!(viewer_out.contains("already_running"), "got: {viewer_out}");
    #[cfg(not(feature = "gui"))]
    {
        assert!(viewer_out.starts_with("[Error]"), "got: {viewer_out}");
        assert!(viewer_out.contains("gui") && viewer_out.contains("[Подсказка]"), "got: {viewer_out}");
    }
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
