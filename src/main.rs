//! Точка входа в бинарник `ob2h.exe`.

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use ob2h::cli::{Cli, Commands, DreamCommands};
use ob2h::config::Settings;
use ob2h::mcp::McpServer;
use ob2h::{init_app, start_background_workers};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::from_env();
    settings.ensure_dirs()?;

    let cli = Cli::parse();
    let is_stdio = matches!(cli.command, None | Some(Commands::Serve));

    // Настройка логирования: в файл всегда, в stderr только если НЕ stdio MCP режим
    let file_appender = tracing_appender::rolling::never(settings.logs_dir(), "ob2h.log");
    let (non_blocking_file, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&settings.log_level));

    if is_stdio {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(non_blocking_file))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .with(tracing_subscriber::fmt::layer().with_writer(non_blocking_file))
            .init();
    }

    let ctx = init_app(settings)?;

    match cli.command {
        None | Some(Commands::Serve) => {
            start_background_workers(ctx.clone());
            let server = McpServer::new(ctx);
            server.run_stdio().await?;
        }
        Some(Commands::Dream { command }) => match command {
            DreamCommands::Run { background } => {
                let output = McpServer::new(ctx)
                    .call_tool("dream_run", serde_json::json!({ "background": background }))
                    .await;
                println!("{output}");
            }
            DreamCommands::Status => {
                let output = McpServer::new(ctx)
                    .call_tool("dream_status", serde_json::json!({}))
                    .await;
                println!("{output}");
            }
            DreamCommands::Log { limit } => {
                let output = McpServer::new(ctx)
                    .call_tool("dream_log", serde_json::json!({ "limit": limit }))
                    .await;
                println!("{output}");
            }
            DreamCommands::Restore { commit } => {
                let output = McpServer::new(ctx)
                    .call_tool("dream_restore", serde_json::json!({ "commit": commit }))
                    .await;
                println!("{output}");
            }
        },
        Some(Commands::Backup) => {
            let output = McpServer::new(ctx)
                .call_tool("omnes_backup", serde_json::json!({}))
                .await;
            println!("{output}");
        }
        Some(Commands::Stats) => {
            let output = McpServer::new(ctx)
                .call_tool("omnes_stats", serde_json::json!({}))
                .await;
            println!("{output}");
        }
        Some(Commands::Install) => {
            install_to_hermes()?;
        }
        Some(Commands::Uninstall) => {
            uninstall_from_hermes()?;
        }
    }

    Ok(())
}

fn get_hermes_config_path() -> std::path::PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:\\Users\\ipres\\AppData\\Local".to_string());
    std::path::PathBuf::from(local_app_data).join("hermes").join("config.yaml")
}

fn remove_ob2h_block(yaml: &str) -> String {
    let mut result = Vec::new();
    let mut in_ob2h = false;

    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if line.starts_with("  ob2h:") || line.starts_with("  \"ob2h\":") || line.starts_with("  'ob2h':") {
            in_ob2h = true;
            continue;
        }

        if in_ob2h {
            if line.starts_with("    ") || trimmed.is_empty() {
                // Внутри блока ob2h
                continue;
            } else {
                in_ob2h = false;
            }
        }

        result.push(line);
    }

    result.join("\n")
}

fn install_to_hermes() -> anyhow::Result<()> {
    let config_path = get_hermes_config_path();
    if !config_path.exists() {
        anyhow::bail!("Конфиг Hermes не найден: {}", config_path.display());
    }

    let exe_path = std::env::current_exe()?;
    let exe_str = exe_path.to_string_lossy().replace('\\', "/");
    let current_dir = std::env::current_dir()?;
    let data_dir = current_dir.join("data").to_string_lossy().replace('\\', "/");

    // Бэкап
    let backup_path = config_path.with_extension("yaml.bak");
    std::fs::copy(&config_path, &backup_path)?;
    println!("Создана резервная копия: {}", backup_path.display());

    let content = std::fs::read_to_string(&config_path)?;
    let cleaned = remove_ob2h_block(&content);

    let block = format!(
        "  ob2h:\n    command: \"{}\"\n    args:\n      - \"serve\"\n    env:\n      OB2H_DATA_DIR: \"{}\"\n      OB2H_LLM_BASE_URL: \"https://api.deepseek.com/v1\"\n      OB2H_LLM_API_KEY: \"DEEPSEEK_API_KEY\"\n      OB2H_LLM_MODEL: \"deepseek-v4-flash\"\n      OB2H_EMBED_PROVIDER: \"local\"\n      OB2H_AUTODREAM_ENABLED: \"true\"",
        exe_str, data_dir
    );

    let new_content = if cleaned.contains("mcp_servers:") {
        cleaned.replace("mcp_servers:", &format!("mcp_servers:\n{block}"))
    } else {
        format!("{}\n\nmcp_servers:\n{}\n", cleaned.trim_end(), block)
    };

    std::fs::write(&config_path, new_content)?;
    println!("OB2H успешно зарегистрирован в Hermes ({})!", config_path.display());
    println!("Перезапустите Hermes для активации инструментов.");
    Ok(())
}

fn uninstall_from_hermes() -> anyhow::Result<()> {
    let config_path = get_hermes_config_path();
    if !config_path.exists() {
        anyhow::bail!("Конфиг Hermes не найден: {}", config_path.display());
    }

    let backup_path = config_path.with_extension("yaml.bak");
    std::fs::copy(&config_path, &backup_path)?;

    let content = std::fs::read_to_string(&config_path)?;
    if !content.contains("ob2h:") {
        println!("OB2H не найден в {}.", config_path.display());
        return Ok(());
    }

    let cleaned = remove_ob2h_block(&content);
    let mut new_content = cleaned;
    if new_content.trim_end().ends_with("mcp_servers:") {
        new_content = new_content.replace("mcp_servers:", "").trim_end().to_string();
    }

    std::fs::write(&config_path, new_content)?;
    println!("OB2H успешно удалён из конфига Hermes ({}).", config_path.display());
    Ok(())
}
