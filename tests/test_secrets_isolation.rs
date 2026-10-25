//! Тест изоляции секретов (Фаза 59):
//! - Секреты НЕ попадают в экспорт синка (sync export) (59.1)
//! - Секреты НЕ попадают в memory_search / build_context / graph_search (59.2)
//! - omnes_stats содержит records без значений (59.3)

use std::io::Read;
use flate2::read::GzDecoder;
use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::McpServer;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn test_secrets_isolation_from_sync_and_search() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let mut settings = Settings::default();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();

    let ctx = init_app(settings)?;
    let server = McpServer::new(ctx.clone());

    // 1. Инициализируем и сохраняем секрет
    ctx.secrets.init("master_pin_5555", Some(ob2h::secrets::KdfParams { m_cost: 1024, t_cost: 1, p_cost: 1 }))?;
    ctx.secrets.unlock("master_pin_5555")?;
    ctx.secrets.set("SUPER_SECRET_TOKEN", "ghp_my_ultra_private_github_token_999", Some("GitHub Personal Token"))?;

    // 2. Проверяем, что memory_search не находит секрет
    let search_res = server.call_tool("memory_search", json!({ "query": "ultra_private_github_token" })).await;
    assert!(!search_res.contains("ghp_my_ultra_private_github_token_999"));
    assert!(!search_res.contains("SUPER_SECRET_TOKEN"));

    // 3. Проверяем, что memory_context не включает секрет
    let ctx_res = server.call_tool("memory_context", json!({ "query": "ultra_private_github_token" })).await;
    assert!(!ctx_res.contains("ghp_my_ultra_private_github_token_999"));

    // 4. Проверяем, что graph_search не находит секрет
    let graph_res = server.call_tool("graph_search", json!({ "query": "SUPER_SECRET_TOKEN" })).await;
    assert!(!graph_res.contains("ghp_my_ultra_private_github_token_999"));

    // 5. Проверяем omnes_stats
    let stats_res = server.call_tool("omnes_stats", json!({})).await;
    assert!(stats_res.contains("secrets: records=1"));
    assert!(!stats_res.contains("ghp_my_ultra_private_github_token_999"));

    // 6. Проверяем sync export: бандл НЕ должен содержать секретов
    let bundle_path = ctx.sync.export("vps")?;
    let gz_file = std::fs::File::open(&bundle_path)?;
    let mut decoder = GzDecoder::new(gz_file);
    let mut bundle_content = String::new();
    decoder.read_to_string(&mut bundle_content)?;

    assert!(!bundle_content.contains("ghp_my_ultra_private_github_token_999"));
    assert!(!bundle_content.contains("SUPER_SECRET_TOKEN"));

    Ok(())
}
