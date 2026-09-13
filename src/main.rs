//! Точка входа в бинарник `ob2h.exe`.

use clap::Parser;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use ob2h::cli::{
    BenchCommands, Cli, Commands, DbCommands, DreamCommands, PluginCommands, SyncCommands,
    Vec0Commands,
};
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

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&settings.log_level));

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
        Some(Commands::Bench {
            command,
            mode,
            k,
            golden,
            json,
            save_baseline,
        }) => {
            if let Some(BenchCommands::History { last }) = command {
                ob2h::cli::bench::cli_history(&ctx.settings, last)?;
            } else {
                ob2h::cli::bench::cli_run(&ctx, &mode, &k, golden.as_deref(), json, save_baseline)
                    .await?;
            }
        }
        Some(Commands::Doctor { fix }) => {
            let doctor = ob2h::cli::Doctor::new(ctx.settings.clone(), fix).with_db(ctx.db.clone());
            doctor.run()?;
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
        Some(Commands::Backup { scope, verify }) => {
            if let Some(path) = verify {
                let report = ctx.backup.verify(std::path::Path::new(&path))?;
                println!("{report}");
            } else {
                let target = match scope.as_str() {
                    "quick" => ctx.backup.create_quick()?,
                    "full" => ctx.backup.create()?,
                    other => anyhow::bail!("scope должен быть full|quick, получено: {other}"),
                };
                println!("Бэкап создан: {}", target.display());
            }
        }
        Some(Commands::Db { command }) => match command {
            DbCommands::QuantizeEmbeddings { dry_run } => {
                ob2h::cli::db::run_quantize(&ctx, dry_run)?;
            }
        },
        Some(Commands::Memory { command }) => match command {
            ob2h::cli::MemoryCommands::Dedup { dry_run } => {
                let pairs = ob2h::cli::dedup::collect_pairs(&ctx.db)?;
                if !dry_run {
                    let marked = ob2h::cli::dedup::apply_markers(&ctx.db, &pairs)?;
                    if marked > 0 {
                        println!("помечено записей merge_candidate: {marked}");
                    }
                }
                ob2h::cli::dedup::print_report(&pairs, dry_run);
            }
        },
        Some(Commands::Ralph { command }) => match command {
            ob2h::cli::RalphCommands::FindingsToMemory { project, dry_run } => {
                // FR-K12: verified-findings циклов → долговременная память
                let findings = ctx.ralph.verified_findings(&project)?;
                println!("verified-findings проекта '{project}': {}", findings.len());
                if dry_run {
                    for (content, symbols) in &findings {
                        println!("- {content} {symbols}");
                    }
                    println!("dry-run: ничего не сохранено");
                } else {
                    let mut saved = 0usize;
                    for (content, symbols) in &findings {
                        let meta = format!(r#"{{"symbols":{symbols},"origin":"ralph"}}"#);
                        if ctx
                            .memory
                            .save(content, None, "ralph", 0.7, "dream", Some(&meta))
                            .await
                            .is_ok()
                        {
                            saved += 1;
                        }
                    }
                    println!("перенесено в память: {saved}/{}", findings.len());
                }
            }
        },
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
        Some(Commands::Sync { command }) => match command {
            SyncCommands::Status => {
                println!("{}", ctx.sync.status());
            }
            SyncCommands::Export { peer, full } => {
                let path = ctx.sync.export_opts(&peer, full)?;
                println!(
                    "export{}: {}",
                    if full { " (full)" } else { "" },
                    path.display()
                );
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
            SyncCommands::Push { peer, full } => {
                let path = ctx.sync.push_opts(&peer, full)?;
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
            SyncCommands::Verify { peer } => {
                let report = ctx.sync.verify(&peer).await?;
                println!("{report}");
            }
            SyncCommands::LocalStats => {
                let stats = ctx.sync.local_stats()?;
                println!("{}", serde_json::to_string_pretty(&stats)?);
            }
        },
        Some(Commands::Vec0 { command }) => match command {
            Vec0Commands::Build { rebuild } => {
                let dim = ctx.embedder.dim();
                let added = ctx.db.with_conn(|conn| {
                    ob2h::vector::vec0::build_index(conn, dim, ob2h::vector::vec0::mode(), rebuild)
                })?;
                let st = ctx
                    .db
                    .with_conn(|conn| ob2h::vector::vec0::stats(conn, dim))?;
                println!(
                    "vec0: добавлено {added}, проиндексировано {} из {} (dim {dim}, os {})",
                    st.indexed, st.nodes_with_embedding, st.oversample
                );
            }
            Vec0Commands::Stats => {
                let dim = ctx.embedder.dim();
                let st = ctx
                    .db
                    .with_conn(|conn| ob2h::vector::vec0::stats(conn, dim))?;
                println!(
                    "vec0: проиндексировано {} из {} узлов с эмбеддингом (dim {dim}, os {}, флаг {})",
                    st.indexed,
                    st.nodes_with_embedding,
                    st.oversample,
                    if ob2h::vector::vec0::enabled() { "on" } else { "off" }
                );
            }
            Vec0Commands::Recall { k, limit, golden } => {
                run_vec0_recall(&ctx, k, limit, golden.as_deref()).await?;
            }
        },
        Some(Commands::SkillInstall) => {
            skill_install()?;
        }
        Some(Commands::Agent { command }) => match command {
            ob2h::cli::AgentCommands::Install { agent, path } => {
                ob2h::cli::AgentManager::install(agent, path.as_deref())?;
            }
            ob2h::cli::AgentCommands::Status => {
                ob2h::cli::AgentManager::status()?;
            }
        },
        Some(Commands::Project { command }) => match command {
            ob2h::cli::ProjectCliCommands::Init {
                id,
                name,
                path,
                description,
            } => {
                let p = ctx.project.register_project(
                    &id,
                    &name,
                    &path,
                    description.as_deref(),
                    None,
                )?;
                println!(
                    "Проект зарегистрирован: id={} name='{}' root='{}'",
                    p.id, p.name, p.root_path
                );
            }
            ob2h::cli::ProjectCliCommands::Scan { id, path } => {
                let res = ctx.project.scan_project(&id, path.as_deref(), true)?;
                let _ = ctx.db.with_conn(|conn| {
                    let _ = ob2h::graph::GraphAnalytics::update_god_nodes(conn, &id);
                    Ok(())
                });
                println!(
                    "Сканирование '{}' завершено: файлов={}, узлов={}, связей={}, строк={}",
                    id,
                    res.files_scanned,
                    res.nodes.len(),
                    res.edges.len(),
                    res.lines_total
                );
            }
            ob2h::cli::ProjectCliCommands::List => {
                let list = ctx.project.list_projects()?;
                if list.is_empty() {
                    println!("Зарегистрированных проектов нет. Используйте 'ob2h project init'");
                } else {
                    println!("Зарегистрированные проекты ({}):", list.len());
                    for p in list {
                        let last = p.last_scanned_at.as_deref().unwrap_or("никогда");
                        println!(
                            "- [{}] '{}' ({}) | последний скан: {}",
                            p.id, p.name, p.root_path, last
                        );
                    }
                }
            }
            ob2h::cli::ProjectCliCommands::Report { id } => {
                let report = ctx.db.with_conn(|conn| {
                    ob2h::graph::GraphAnalytics::generate_project_report(conn, &id)
                })?;
                println!("{}", report.markdown_summary);
            }
            ob2h::cli::ProjectCliCommands::RepoMap {
                id,
                tokens,
                query,
                memory,
            } => {
                let map = ctx.db.with_conn(|conn| {
                    ob2h::graph::repomap::build_repo_map(
                        conn,
                        &id,
                        query.as_deref(),
                        tokens,
                        memory,
                    )
                })?;
                println!("{map}");
            }
            ob2h::cli::ProjectCliCommands::DeadCode { id, limit } => {
                let dead = ctx
                    .db
                    .with_conn(|conn| ob2h::graph::callpath::dead_code(conn, &id))?;
                println!("{}", ob2h::graph::callpath::format_dead_code(&dead, limit));
            }
            ob2h::cli::ProjectCliCommands::HookInstall { path, id } => {
                let target_path = if let Some(p) = path {
                    std::path::PathBuf::from(p)
                } else {
                    std::env::current_dir()?
                };
                let project_root = ob2h::project::find_project_root(&target_path);
                let project_id = if let Some(i) = id {
                    i
                } else {
                    let p = ctx.project.auto_register_or_detect(&project_root)?;
                    p.id
                };
                let installed = ob2h::project::install_git_hooks(&project_root, &project_id)?;
                println!(
                    "Установлены Git-хуки для проекта '{}' в {}: {:?}",
                    project_id,
                    project_root.display(),
                    installed
                );
            }
        },
    }

    Ok(())
}

fn format_import_stats(stats: &ob2h::sync::ImportStats) -> String {
    if stats.already_applied {
        return format!("{}: уже применён (no-op)", stats.bundle_id);
    }
    format!(
        "{}: mem={} node={} edge={} mlink={} конфликтов_проиграно={} в_журнале={} пропусков_ссылок={}",
        stats.bundle_id, stats.memories_applied, stats.nodes_applied, stats.edges_applied,
        stats.links_applied, stats.conflicts_lost, stats.conflicts_journaled,
        stats.skipped_missing_ref
    )
}

/// Ф35.1: recall@k vec0+рескоринг против полного перебора на golden-запросах.
/// Критерий приёмки — recall@10 ≥ 0.99; печатает recall, число запросов и индекса.
async fn run_vec0_recall(
    ctx: &ob2h::mcp::AppContext,
    k: usize,
    limit: Option<usize>,
    golden: Option<&str>,
) -> anyhow::Result<()> {
    use ob2h::cli::bench::load_golden;

    let golden_path = match golden {
        Some(p) => std::path::PathBuf::from(p),
        None => ctx.settings.data_dir.join("bench").join("golden.jsonl"),
    };
    let mut cases = load_golden(&golden_path)?;
    if let Some(n) = limit {
        cases.truncate(n);
    }
    let dim = ctx.embedder.dim();
    let os = ob2h::vector::vec0::oversample();
    let st = ctx.db.with_conn(|conn| {
        ob2h::vector::vec0::stats(conn, dim)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
    })?;
    if st.indexed == 0 {
        anyhow::bail!("vec0-индекс пуст — сначала `ob2h vec0 build`");
    }

    let mut recalls = Vec::new();
    let mut t_brute = 0.0f64;
    let mut t_vec0 = 0.0f64;
    for case in &cases {
        let embs = ctx
            .embedder
            .embed(std::slice::from_ref(&case.query))
            .await?;
        let Some(q) = embs.first() else { continue };

        // ground truth: полный перебор (прежний путь)
        let brute: Vec<i64> = {
            let started = std::time::Instant::now();
            let cands = ctx.db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, embedding FROM graph_nodes WHERE embedding IS NOT NULL AND deleted_at IS NULL",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                })?;
                let mut list = Vec::new();
                for r in rows.flatten() {
                    list.push(r);
                }
                Ok(list)
            })?;
            let refs: Vec<(i64, Option<&[u8]>)> = cands
                .iter()
                .map(|(id, b)| (*id, Some(b.as_slice())))
                .collect();
            let out: Vec<i64> = ob2h::vector::top_k(q, &refs, k, 0.0)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            t_brute += started.elapsed().as_secs_f64() * 1000.0;
            out
        };

        let approx: Vec<i64> = {
            let started = std::time::Instant::now();
            let hits = ctx
                .db
                .with_conn(|conn| ob2h::vector::vec0::search(conn, q, k, os))?;
            t_vec0 += started.elapsed().as_secs_f64() * 1000.0;
            hits.into_iter().map(|(id, _)| id).collect()
        };

        recalls.push(ob2h::vector::vec0::recall_at_k(&brute, &approx, k));
    }

    let n = recalls.len().max(1) as f64;
    let recall = recalls.iter().sum::<f64>() / n;
    println!(
        "vec0 recall@{k} = {recall:.4} (запросов {}, os {os}, индекс {} из {})",
        recalls.len(),
        st.indexed,
        st.nodes_with_embedding
    );
    println!(
        "средняя латентность: перебор {:.1} мс, vec0+рескоринг {:.1} мс (без эмбеддинга запроса)",
        t_brute / n,
        t_vec0 / n
    );
    println!(
        "критерий приёмки recall@{k} ≥ 0.99: {}",
        if recall >= 0.99 {
            "ВЫПОЛНЕН"
        } else {
            "НЕ выполнен"
        }
    );
    Ok(())
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
    println!(
        "Включите провайдер вручную в {}:",
        home.join("config.yaml").display()
    );
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
    let installed = PLUGIN_FILES
        .iter()
        .all(|(name, _)| dir.join(name).is_file());
    println!("hermes_home: {}", home.display());
    println!(
        "plugin_dir:  {} [{}]",
        dir.display(),
        if installed {
            "установлен"
        } else {
            "не установлен"
        }
    );
    let cfg_path = home.join("config.yaml");
    if let Ok(content) = std::fs::read_to_string(&cfg_path) {
        let active = content.lines().any(|l| l.trim() == "provider: ob2h");
        println!(
            "memory.provider: ob2h — {}",
            if active {
                "включён в конфиге"
            } else {
                "не найден в config.yaml"
            }
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
        println!(
            "ob2h.json:    {} (пути бинарника/data_dir плагина)",
            ob2h_json.display()
        );
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
    println!(
        "Пути: binary={}, data_dir={}",
        exe.display(),
        data_dir.display()
    );
    Ok(())
}

fn get_hermes_config_path() -> std::path::PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA")
        .unwrap_or_else(|_| "C:\\Users\\ipres\\AppData\\Local".to_string());
    std::path::PathBuf::from(local_app_data)
        .join("hermes")
        .join("config.yaml")
}

fn remove_ob2h_block(yaml: &str) -> String {
    let mut result = Vec::new();
    let mut in_ob2h = false;

    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if line.starts_with("  ob2h:")
            || line.starts_with("  \"ob2h\":")
            || line.starts_with("  'ob2h':")
        {
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
    let data_dir = current_dir
        .join("data")
        .to_string_lossy()
        .replace('\\', "/");

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
    println!(
        "OB2H успешно зарегистрирован в Hermes ({})!",
        config_path.display()
    );
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
        new_content = new_content
            .replace("mcp_servers:", "")
            .trim_end()
            .to_string();
    }

    std::fs::write(&config_path, new_content)?;
    println!(
        "OB2H успешно удалён из конфига Hermes ({}).",
        config_path.display()
    );
    Ok(())
}
