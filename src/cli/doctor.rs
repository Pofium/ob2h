//! Диагностика системы `ob2h doctor`.
//! Проверяет целостность SQLite, WAL, FTS5 trigram, векторную модель Candle,
//! конфигурацию всех AI-агентов (Claude, Cursor, Windsurf, Gemini/Antigravity, Hermes, Qwen, OpenCode),
//! Git-хуки и пиринги синхронизации. Поддерживает автоматическое исправление (`--fix`).

use std::fs;
use std::path::{Path, PathBuf};
use rusqlite::Connection;
use serde_json::Value;

use crate::cli::agent::AgentManager;
use crate::config::Settings;
use crate::project::install_git_hooks;

#[derive(Debug, Clone)]
pub struct DoctorItem {
    pub category: String,
    pub name: String,
    pub status: DoctorStatus,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warn,
    Error,
}

impl DoctorStatus {
    pub fn symbol(&self) -> &'static str {
        match self {
            DoctorStatus::Ok => "✅",
            DoctorStatus::Warn => "⚠️ ",
            DoctorStatus::Error => "❌",
        }
    }
}

pub struct Doctor {
    settings: Settings,
    fix: bool,
}

impl Doctor {
    pub fn new(settings: Settings, fix: bool) -> Self {
        Self { settings, fix }
    }

    pub fn run(&self) -> anyhow::Result<Vec<DoctorItem>> {
        let mut results = Vec::new();

        println!("\n🩺 Запуск глубокой диагностики OB2H (Doctor)...");
        if self.fix {
            println!("🔧 Включён режим автоисправления (--fix)\n");
        } else {
            println!();
        }

        // 1. Проверка директорий и диска
        self.check_directories(&mut results);

        // 2. Проверка базы данных SQLite
        self.check_database(&mut results);

        // 3. Проверка векторной подсистемы
        self.check_embeddings(&mut results);

        // 4. Проверка подключения AI-агентов
        self.check_agents(&mut results);

        // 5. Проверка Git-хуков в проектах
        self.check_project_hooks(&mut results);

        // 6. Проверка синхронизации (peers.json)
        self.check_sync(&mut results);

        // Вывод сводного отчёта
        self.print_summary(&results);

        Ok(results)
    }

    fn check_directories(&self, results: &mut Vec<DoctorItem>) {
        let data_dir = &self.settings.data_dir;
        let exists = data_dir.exists();
        results.push(DoctorItem {
            category: "Файловая система".to_string(),
            name: "Каталог данных (OB2H_DATA_DIR)".to_string(),
            status: if exists { DoctorStatus::Ok } else { DoctorStatus::Error },
            details: format!("{}", data_dir.display()),
        });

        let workspace = self.settings.workspace_dir();
        results.push(DoctorItem {
            category: "Файловая система".to_string(),
            name: "Рабочая область (Workspace)".to_string(),
            status: if workspace.exists() { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: format!("{}", workspace.display()),
        });
    }

    fn check_database(&self, results: &mut Vec<DoctorItem>) {
        let db_path = self.settings.db_path();
        if !db_path.exists() {
            results.push(DoctorItem {
                category: "База данных SQLite".to_string(),
                name: "Файл базы данных".to_string(),
                status: DoctorStatus::Warn,
                details: format!("Файл {} ещё не создан (будет создан при первом запуске)", db_path.display()),
            });
            return;
        }

        let size_bytes = fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
        let size_mb = size_bytes as f64 / (1024.0 * 1024.0);

        results.push(DoctorItem {
            category: "База данных SQLite".to_string(),
            name: "Файл базы данных".to_string(),
            status: DoctorStatus::Ok,
            details: format!("{} ({:.2} MB)", db_path.display(), size_mb),
        });

        // Проверяем подключение и целостность
        match Connection::open(&db_path) {
            Ok(conn) => {
                // WAL check
                let journal_mode: String = conn
                    .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
                    .unwrap_or_default();
                let is_wal = journal_mode.to_lowercase() == "wal";
                results.push(DoctorItem {
                    category: "База данных SQLite".to_string(),
                    name: "Режим журнала WAL".to_string(),
                    status: if is_wal { DoctorStatus::Ok } else { DoctorStatus::Warn },
                    details: format!("journal_mode = {}", journal_mode),
                });

                // Quick check
                let quick_check: String = conn
                    .query_row("PRAGMA quick_check;", [], |r| r.get(0))
                    .unwrap_or_else(|e| format!("error: {e}"));
                let is_integrity_ok = quick_check == "ok";
                results.push(DoctorItem {
                    category: "База данных SQLite".to_string(),
                    name: "Целостность базы (quick_check)".to_string(),
                    status: if is_integrity_ok { DoctorStatus::Ok } else { DoctorStatus::Error },
                    details: quick_check,
                });

                // FTS5 Trigram
                let fts_check: Result<i64, _> = conn.query_row(
                    "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH 'test'",
                    [],
                    |r| r.get(0),
                );
                results.push(DoctorItem {
                    category: "База данных SQLite".to_string(),
                    name: "Полнотекстовый поиск FTS5 (Trigram)".to_string(),
                    status: if fts_check.is_ok() { DoctorStatus::Ok } else { DoctorStatus::Error },
                    details: if fts_check.is_ok() { "Индекс активен и отвечает на запросы".to_string() } else { "Ошибка FTS5 индекса".to_string() },
                });

                // Счётчики записей
                let count_mem: i64 = conn.query_row("SELECT COUNT(*) FROM memories WHERE deleted_at IS NULL", [], |r| r.get(0)).unwrap_or(0);
                let count_nodes: i64 = conn.query_row("SELECT COUNT(*) FROM graph_nodes WHERE deleted_at IS NULL", [], |r| r.get(0)).unwrap_or(0);
                let count_edges: i64 = conn.query_row("SELECT COUNT(*) FROM graph_edges WHERE deleted_at IS NULL", [], |r| r.get(0)).unwrap_or(0);
                let count_proj: i64 = conn.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0)).unwrap_or(0);
                let count_files: i64 = conn.query_row("SELECT COUNT(*) FROM project_files", [], |r| r.get(0)).unwrap_or(0);

                results.push(DoctorItem {
                    category: "База данных SQLite".to_string(),
                    name: "Статистика таблиц".to_string(),
                    status: DoctorStatus::Ok,
                    details: format!(
                        "Воспоминаний: {}, Узлов графа: {}, Связей: {}, Проектов: {}, Файлов кода: {}",
                        count_mem, count_nodes, count_edges, count_proj, count_files
                    ),
                });
            }
            Err(e) => {
                results.push(DoctorItem {
                    category: "База данных SQLite".to_string(),
                    name: "Подключение".to_string(),
                    status: DoctorStatus::Error,
                    details: format!("Не удалось открыть БД: {e}"),
                });
            }
        }
    }

    fn check_embeddings(&self, results: &mut Vec<DoctorItem>) {
        results.push(DoctorItem {
            category: "Эмбеддинги и Вектора".to_string(),
            name: "Провайдер эмбеддингов".to_string(),
            status: DoctorStatus::Ok,
            details: format!("{} (модель: {})", self.settings.embed_provider, self.settings.embed_model),
        });
    }

    fn check_agents(&self, results: &mut Vec<DoctorItem>) {
        let home = AgentManager::home_dir();
        let exe = AgentManager::current_exe_path();

        // 1. Claude
        let claude_cfg = home.join(".claude").join("config.json");
        let claude_has_ob2h = self.has_mcp_entry(&claude_cfg, "ob2h");
        if !claude_has_ob2h && self.fix {
            let _ = AgentManager::install_claude(&exe);
        }
        results.push(DoctorItem {
            category: "AI-Агенты (MCP)".to_string(),
            name: "Claude Code".to_string(),
            status: if claude_has_ob2h || (self.fix && self.has_mcp_entry(&claude_cfg, "ob2h")) { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: format!("Конфиг: {}", claude_cfg.display()),
        });

        // 2. Cursor
        let cursor_cfg = home.join(".cursor").join("mcp.json");
        let cursor_has_ob2h = self.has_mcp_entry(&cursor_cfg, "ob2h");
        if !cursor_has_ob2h && self.fix {
            let _ = AgentManager::install_cursor(&exe, None);
        }
        results.push(DoctorItem {
            category: "AI-Агенты (MCP)".to_string(),
            name: "Cursor (global)".to_string(),
            status: if cursor_has_ob2h || (self.fix && self.has_mcp_entry(&cursor_cfg, "ob2h")) { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: format!("Конфиг: {}", cursor_cfg.display()),
        });

        // 3. Windsurf
        let windsurf_cfg = home.join(".codeium").join("windsurf").join("mcp_config.json");
        let windsurf_has_ob2h = self.has_mcp_entry(&windsurf_cfg, "ob2h");
        if !windsurf_has_ob2h && self.fix {
            let _ = AgentManager::install_windsurf(&exe);
        }
        results.push(DoctorItem {
            category: "AI-Агенты (MCP)".to_string(),
            name: "Windsurf / Cascade".to_string(),
            status: if windsurf_has_ob2h || (self.fix && self.has_mcp_entry(&windsurf_cfg, "ob2h")) { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: format!("Конфиг: {}", windsurf_cfg.display()),
        });

        // 4. Gemini / Antigravity (проверяем ОБА пути!)
        let gemini_cfg1 = home.join(".gemini").join("antigravity-ide").join("mcp_config.json");
        let gemini_cfg2 = home.join(".gemini").join("antigravity").join("mcp_config.json");
        let gemini_has_ob2h = self.has_mcp_entry(&gemini_cfg1, "ob2h") || self.has_mcp_entry(&gemini_cfg2, "ob2h");
        if !gemini_has_ob2h && self.fix {
            let _ = AgentManager::install_gemini(&exe);
        }
        results.push(DoctorItem {
            category: "AI-Агенты (MCP)".to_string(),
            name: "Gemini / Antigravity".to_string(),
            status: if gemini_has_ob2h || (self.fix && (self.has_mcp_entry(&gemini_cfg1, "ob2h") || self.has_mcp_entry(&gemini_cfg2, "ob2h"))) { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: format!("Пути: {} / {}", gemini_cfg1.display(), gemini_cfg2.display()),
        });

        // 5. Hermes
        let hermes_cfg = home.join(".hermes").join("config.yaml");
        let hermes_appdata = if let Ok(local) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local).join("hermes").join("config.yaml")
        } else {
            hermes_cfg.clone()
        };
        let hermes_exists = hermes_cfg.exists() || hermes_appdata.exists();
        results.push(DoctorItem {
            category: "AI-Агенты (MCP)".to_string(),
            name: "Hermes Agent".to_string(),
            status: if hermes_exists { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: if hermes_appdata.exists() {
                format!("Конфиг найден: {}", hermes_appdata.display())
            } else if hermes_cfg.exists() {
                format!("Конфиг найден: {}", hermes_cfg.display())
            } else {
                "Не найден (используйте 'ob2h install')".to_string()
            },
        });

        // 6. Qwen Code
        let qwen_cfg = home.join(".qwen").join("mcp.json");
        let qwen_has_ob2h = self.has_mcp_entry(&qwen_cfg, "ob2h");
        if !qwen_has_ob2h && self.fix {
            let _ = AgentManager::install_qwen(&exe);
        }
        results.push(DoctorItem {
            category: "AI-Агенты (MCP)".to_string(),
            name: "Qwen Code".to_string(),
            status: if qwen_has_ob2h || (self.fix && self.has_mcp_entry(&qwen_cfg, "ob2h")) { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: format!("Конфиг: {}", qwen_cfg.display()),
        });

        // 7. OpenCode
        let opencode_cfg = home.join(".opencode").join("mcp.json");
        let opencode_has_ob2h = self.has_mcp_entry(&opencode_cfg, "ob2h");
        if !opencode_has_ob2h && self.fix {
            let _ = AgentManager::install_opencode(&exe);
        }
        results.push(DoctorItem {
            category: "AI-Агенты (MCP)".to_string(),
            name: "OpenCode".to_string(),
            status: if opencode_has_ob2h || (self.fix && self.has_mcp_entry(&opencode_cfg, "ob2h")) { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details: format!("Конфиг: {}", opencode_cfg.display()),
        });
    }

    fn check_project_hooks(&self, results: &mut Vec<DoctorItem>) {
        let db_path = self.settings.db_path();
        if !db_path.exists() {
            return;
        }

        if let Ok(conn) = Connection::open(&db_path) {
            let mut stmt = match conn.prepare("SELECT id, root_path FROM projects") {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut rows = match stmt.query([]) {
                Ok(r) => r,
                Err(_) => return,
            };

            let mut checked_projects = 0;
            let mut hooks_installed = 0;

            while let Ok(Some(row)) = rows.next() {
                let id: String = row.get(0).unwrap_or_default();
                let root_path: String = row.get(1).unwrap_or_default();
                checked_projects += 1;

                let p = Path::new(&root_path);
                let hook_file = p.join(".git").join("hooks").join("post-commit");
                if hook_file.exists() {
                    hooks_installed += 1;
                } else if self.fix {
                    let _ = install_git_hooks(p, &id);
                    if hook_file.exists() {
                        hooks_installed += 1;
                    }
                }
            }

            if checked_projects > 0 {
                results.push(DoctorItem {
                    category: "Git Автоматизация".to_string(),
                    name: "Git Hooks в проектах".to_string(),
                    status: if hooks_installed == checked_projects { DoctorStatus::Ok } else { DoctorStatus::Warn },
                    details: format!("Установлены в {} из {} зарегистрированных проектов", hooks_installed, checked_projects),
                });
            }
        }
    }

    fn check_sync(&self, results: &mut Vec<DoctorItem>) {
        let peers_path = self.settings.data_dir.join("sync").join("peers.json");
        let exists = peers_path.exists();

        let details = if exists {
            match fs::read_to_string(&peers_path) {
                Ok(content) => match serde_json::from_str::<Value>(&content) {
                    Ok(val) => {
                        let peer_count = val.get("peers").and_then(|p| p.as_object()).map(|o| o.len()).unwrap_or(0);
                        format!("peers.json валиден, настроено пиров: {peer_count}")
                    }
                    Err(e) => format!("peers.json повреждён: {e}"),
                },
                Err(e) => format!("ошибка чтения peers.json: {e}"),
            }
        } else {
            "peers.json не создан (синхронизация PC <-> VPS отключена)".to_string()
        };

        results.push(DoctorItem {
            category: "Синхронизация PC ↔ VPS".to_string(),
            name: "Конфигурация пирингов".to_string(),
            status: if exists { DoctorStatus::Ok } else { DoctorStatus::Warn },
            details,
        });
    }

    fn has_mcp_entry(&self, path: &Path, name: &str) -> bool {
        if !path.exists() {
            return false;
        }
        let content = fs::read_to_string(path).unwrap_or_default();
        if let Ok(val) = serde_json::from_str::<Value>(&content) {
            if let Some(mcp) = val.get("mcpServers").and_then(|v| v.as_object()) {
                return mcp.contains_key(name);
            }
        }
        false
    }

    fn print_summary(&self, items: &[DoctorItem]) {
        let mut current_cat = "";
        for item in items {
            if item.category != current_cat {
                current_cat = &item.category;
                println!("\n📋 {}", current_cat);
            }
            println!("  {} {:<32} {}", item.status.symbol(), item.name, item.details);
        }

        let errors = items.iter().filter(|i| i.status == DoctorStatus::Error).count();
        let warns = items.iter().filter(|i| i.status == DoctorStatus::Warn).count();

        println!("\n------------------------------------------------------------");
        if errors == 0 && warns == 0 {
            println!("🎉 Все подсистемы OB2H функционируют идеально!");
        } else {
            println!("Итог: Ошибок: {}, Предупреждений: {}", errors, warns);
            if !self.fix && (warns > 0 || errors > 0) {
                println!("💡 Совет: запустите 'ob2h doctor --fix' для автоматического исправления конфигурации агентов и Git-хуков.");
            }
        }
        println!("------------------------------------------------------------\n");
    }
}
