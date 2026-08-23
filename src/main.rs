//! Точка входа в бинарник `ob2h.exe`.

use clap::Parser;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use ob2h::cli::{Cli, Commands, DreamCommands, PluginCommands, SyncCommands};
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
            let server = Arc::new(McpServer::new(ctx));
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
        Some(Commands::Plugin { command }) => match command {
            PluginCommands::Install => plugin_install()?,
            PluginCommands::Uninstall => plugin_uninstall()?,
            PluginCommands::Status => plugin_status()?,
        },
        Some(Commands::Sync { command }) => match command {            SyncCommands::Status => {
                println!("{}", ctx.sync.status());
            }
            SyncCommands::Export { peer } => {
                let path = ctx.sync.export(&peer)?;
                println!("export: {}", path.display());
            }
            SyncCommands::Import { files } => {
                if files.is_empty() {
                    anyhow::bail!("укажите пути к бандлам: ob2h sync import <file...>");
                }
                for f in &files {
                    let stats = ctx.sync.import_file(std::path::Path::new(f)).await?;
                    println!("{f}: {}", format_import_stats(&stats));
                }
            }
            SyncCommands::ApplyInbox => {
                let all = ctx.sync.apply_inbox().await?;
                if all.is_empty() {
                    println!("inbox пуст");
                }
                for stats in all {
                    println!("{}", format_import_stats(&stats));
                }
            }
            SyncCommands::Push { peer } => {
                let path = ctx.sync.push(&peer)?;
                println!("push: {} → {peer}", path.display());
            }
            SyncCommands::Pull { peer } => {
                let all = ctx.sync.pull(&peer).await?;
                if all.is_empty() {
                    println!("от {peer} новых бандлов нет");
                }
                for stats in all {
                    println!("{}", format_import_stats(&stats));
                }
            }
        },
        Some(Commands::SkillInstall) => {
            skill_install()?;
        }
    }

    Ok(())
}

fn format_import_stats(stats: &ob2h::sync::ImportStats) -> String {
    if stats.already_applied {
        return format!("{}: уже применён (no-op)", stats.bundle_id);
    }
    format!(
        "{}: mem={} node={} edge={} конфликтов_проиграно={} пропусков_ссылок={}",
        stats.bundle_id, stats.memories_applied, stats.nodes_applied, stats.edges_applied,
        stats.conflicts_lost, stats.skipped_missing_ref
    )
}

// -- MemoryProvider-плагин (docs/PLAN_v0.8.md §7.3) -------------------------

const PLUGIN_FILES: &[(&str, &str)] = &[
    ("__init__.py", include_str!("../plugin/ob2h/__init__.py")),
    ("_rpc.py", include_str!("../plugin/ob2h/_rpc.py")),
    ("plugin.yaml", include_str!("../plugin/ob2h/plugin.yaml")),
];

fn get_hermes_home() -> anyhow::Result<std::path::PathBuf> {
    if let Ok(h) = std::env::var("HERMES_HOME") {
        return Ok(std::path::PathBuf::from(h));
    }
    // Windows: %LOCALAPPDATA%\hermes; Linux (VPS): ~/.hermes
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let candidate = std::path::PathBuf::from(local_app_data).join("hermes");
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if !home.is_empty() {
        let dot = std::path::PathBuf::from(home).join(".hermes");
        if dot.is_dir() {
            return Ok(dot);
        }
    }
    anyhow::bail!("HERMES_HOME не найден: задайте переменную HERMES_HOME");
}

fn plugin_install() -> anyhow::Result<()> {
    let home = get_hermes_home()?;
    let dir = home.join("plugins").join("ob2h");
    std::fs::create_dir_all(&dir)?;
    for (name, content) in PLUGIN_FILES {
        std::fs::write(dir.join(name), content)?;
    }
    println!("Плагин ob2h установлен: {}", dir.display());

    // Плагин стартует из Hermes (cwd = домашняя папка): без пина путей он нашёл бы
    // бинарник/data не там и создал вторую БД. install — явный акт деплоя ЭТОГО
    // бинарника: binary/data_dir актуализируем, прочие ключи ob2h.json сохраняем.
    let cfg_path = home.join("ob2h.json");
    let exe = std::env::current_exe()?;
    let data_dir = match std::env::var("OB2H_DATA_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => std::env::current_dir()?.join("data"),
    };
    let mut cfg: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    cfg.insert(
        "binary".to_string(),
        serde_json::Value::String(exe.to_string_lossy().replace('\\', "/")),
    );
    cfg.insert(
        "data_dir".to_string(),
        serde_json::Value::String(data_dir.to_string_lossy().replace('\\', "/")),
    );
    std::fs::write(
        &cfg_path,
        serde_json::to_string_pretty(&serde_json::Value::Object(cfg))? + "\n",
    )?;
    println!(
        "Пути плагина закреплены в {}: binary={}, data_dir={}",
        cfg_path.display(),
        exe.display(),
        data_dir.display()
    );

    println!();
    println!("Включите провайдер вручную в {}:", home.join("config.yaml").display());
    println!("  memory:");
    println!("    provider: ob2h");
    println!();
    println!("и перезапустите Hermes. Проверка: ob2h plugin status");
    Ok(())
}

fn plugin_uninstall() -> anyhow::Result<()> {
    let home = get_hermes_home()?;
    let dir = home.join("plugins").join("ob2h");
    if !dir.is_dir() {
        println!("Плагин не установлен: {}", dir.display());
        return Ok(());
    }
    std::fs::remove_dir_all(&dir)?;
    println!("Плагин удалён: {}", dir.display());
    println!("Если был включён — уберите `provider: ob2h` из блока memory: в config.yaml Hermes.");
    Ok(())
}

fn plugin_status() -> anyhow::Result<()> {
    let home = get_hermes_home()?;
    let dir = home.join("plugins").join("ob2h");
    let installed = PLUGIN_FILES.iter().all(|(name, _)| dir.join(name).is_file());
    println!("hermes_home: {}", home.display());
    println!(
        "plugin_dir:  {} [{}]",
        dir.display(),
        if installed { "установлен" } else { "не установлен" }
    );
    let cfg_path = home.join("config.yaml");
    if let Ok(content) = std::fs::read_to_string(&cfg_path) {
        let active = content.lines().any(|l| l.trim() == "provider: ob2h");
        println!(
            "memory.provider: ob2h — {}",
            if active { "включён в конфиге" } else { "не найден в config.yaml" }
        );
        let mcp = content.lines().any(|l| l.starts_with("  ob2h:"));
        println!(
            "mcp_servers.ob2h — {} (Mode B, если включён и плагин активен)",
            if mcp { "есть" } else { "нет" }
        );
    } else {
        println!("config.yaml не найден: {}", cfg_path.display());
    }
    let ob2h_json = home.join("ob2h.json");
    if ob2h_json.is_file() {
        println!("ob2h.json:    {} (пути бинарника/data_dir плагина)", ob2h_json.display());
    }
    Ok(())
}

const SKILL_SOURCE: &str = include_str!("../skills/ob2h/SKILL.md");

/// Деплой скилла в $HERMES_HOME/skills/devops/ob2h/SKILL.md с темплейтами
/// путей этой машины (единый исходник для Windows/Linux).
fn skill_install() -> anyhow::Result<()> {
    let home = get_hermes_home()?;
    let exe = std::env::current_exe()?;
    let data_dir = match std::env::var("OB2H_DATA_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => std::env::current_dir()?.join("data"),
    };
    let project_dir = std::env::current_dir()?;

    let skill = SKILL_SOURCE
        .replace("{{BINARY}}", &exe.to_string_lossy())
        .replace("{{DATA_DIR}}", &data_dir.to_string_lossy())
        .replace("{{PROJECT_DIR}}", &project_dir.to_string_lossy())
        .replace("{{HERMES_HOME}}", &home.to_string_lossy())
        .replace("{{STATE_DB}}", &home.join("state.db").to_string_lossy());

    let dir = home.join("skills").join("devops").join("ob2h");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("SKILL.md"), skill)?;
    println!("Скилл ob2h установлен: {}", dir.join("SKILL.md").display());
    println!("Пути: binary={}, data_dir={}", exe.display(), data_dir.display());
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
