//! Ф25: громкий FakeEmbedding, author-фильтр контекста, автор в daily-логе,
//! LWW-счётчик в sync status.

use std::sync::Arc;

use tempfile::tempdir;

use ob2h::config::Settings;
use ob2h::memory::ContextOptions;
use ob2h::mcp::McpServer;
use ob2h::{init_app, mcp::AppContext};

fn fake_settings(tmp: &tempfile::TempDir) -> Settings {
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();
    settings
}

#[tokio::test]
async fn stats_reports_fake_backend_and_search_has_no_warn_when_intentional() {
    let tmp = tempdir().expect("tempdir");
    let ctx = init_app(fake_settings(&tmp)).expect("init app");
    let server = McpServer::new(ctx.clone());

    let stats = server.call_tool("omnes_stats", serde_json::json!({})).await;
    assert!(stats.contains("backend=fake"), "stats показывает бэкенд: {stats}");

    ctx.memory
        .save("Факт для поиска", Some("hmem-p25"), "t", 0.5, "chat", None)
        .await
        .expect("save");
    let out = server
        .call_tool("memory_search", serde_json::json!({ "query": "факт" }))
        .await;
    // fake включён явно — это не деградация, warn не добавляется
    assert!(!out.contains("[warn] эмбеддинги"), "явный fake не деградация: {out}");
}

#[tokio::test]
async fn context_author_filter_excludes_other_author_records() {
    let tmp = tempdir().expect("tempdir");
    let ctx = init_app(fake_settings(&tmp)).expect("init app");

    ctx.memory
        .save(
            "Правило агента А",
            Some("hmem-a25"),
            "t",
            0.9,
            "chat",
            Some(r#"{"author":"agentA"}"#),
        )
        .await
        .expect("save A");
    ctx.memory
        .save("Правило агента Б", Some("hmem-b25"), "t", 0.9, "chat", None)
        .await
        .expect("save B");

    let memory = ctx.memory.clone();
    // Контекст агента A: своя запись остаётся, запись без автора проходит,
    // чужая (в примере — только agentB) исключилась бы.
    let opts_a = ContextOptions { author: Some("agentA".into()), ..Default::default() };
    let block_a = memory.build_context(10, None, &opts_a).await.expect("ctx A");
    assert!(block_a.contains("Правило агента А"));
    assert!(block_a.contains("Правило агента Б"), "записи без автора проходят всем");

    // Контекст агента B: запись A (чужой author) исключается
    let opts_b = ContextOptions { author: Some("agentB".into()), ..Default::default() };
    let block_b = memory.build_context(10, None, &opts_b).await.expect("ctx B");
    assert!(block_b.contains("Правило агента Б"));
    assert!(!block_b.contains("Правило агента А"), "чужая запись исключена: {block_b}");
}

#[tokio::test]
async fn session_log_writes_author_to_daily_meta() {
    let tmp = tempdir().expect("tempdir");
    let ctx = init_app(fake_settings(&tmp)).expect("init app");
    let server = McpServer::new(ctx.clone());

    let out = server
        .call_tool(
            "session_log",
            serde_json::json!({
                "user_text": "вопрос",
                "assistant_text": "ответ",
                "author": "ilya"
            }),
        )
        .await;
    assert!(out.starts_with("logged"), "{out}");

    let daily = ctx.settings.workspace_dir().join("daily");
    let mut found = false;
    for entry in std::fs::read_dir(&daily).expect("daily dir").flatten() {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        if content.contains("\"author\":\"ilya\"") {
            found = true;
        }
    }
    assert!(found, "author должен попасть в meta daily-записи");
}

#[tokio::test]
async fn sync_status_reports_lww_conflict_counter() {
    let tmp = tempdir().expect("tempdir");
    let ctx = init_app(fake_settings(&tmp)).expect("init app");
    let status = ctx.sync.status();
    assert!(
        status.contains("конфликтов LWW проиграно (всего): 0"),
        "новый старт — счётчик нулевой: {status}"
    );
}
