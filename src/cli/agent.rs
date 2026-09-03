//! Мультиагентный менеджер интеграций (Фаза 14).
//! Установка и настройка OB2H для: Claude Code, Cursor, Windsurf, ZCode, Gemini CLI / Antigravity, Qwen Code, OpenCode, Hermes.

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
pub enum AgentTarget {
    All,
    Claude,
    Cursor,
    Windsurf,
    Zcode,
    Gemini,
    Qwen,
    Opencode,
    Hermes,
}

pub struct AgentManager;

impl AgentManager {
    /// Получить текущий путь к исполняемому файлу ob2h.
    pub fn current_exe_path() -> String {
        std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("ob2h"))
            .to_string_lossy()
            .to_string()
    }

    /// Домашняя папка пользователя.
    pub fn home_dir() -> PathBuf {
        if let Ok(p) = std::env::var("USERPROFILE") {
            PathBuf::from(p)
        } else if let Ok(p) = std::env::var("HOME") {
            PathBuf::from(p)
        } else {
            PathBuf::from(".")
        }
    }

    /// Установка интеграции для выбранного агента или для всех сразу.
    pub fn install(target: AgentTarget, custom_path: Option<&str>) -> anyhow::Result<()> {
        let exe = Self::current_exe_path();
        println!("🚀 Установка интеграции OB2H: executable = {}", exe);

        match target {
            AgentTarget::All => {
                let mut installed = 0;
                println!("🔍 Автообнаружение установленных AI-агентов в системе...");

                if Self::install_claude(&exe).is_ok() { installed += 1; }
                if Self::install_cursor(&exe, custom_path).is_ok() { installed += 1; }
                if Self::install_windsurf(&exe).is_ok() { installed += 1; }
                if Self::install_zcode(&exe, custom_path).is_ok() { installed += 1; }
                if Self::install_gemini(&exe).is_ok() { installed += 1; }
                if Self::install_qwen(&exe).is_ok() { installed += 1; }
                if Self::install_opencode(&exe).is_ok() { installed += 1; }

                println!("✨ Готово! Настроено агентов: {}", installed);
            }
            AgentTarget::Claude => Self::install_claude(&exe)?,
            AgentTarget::Cursor => Self::install_cursor(&exe, custom_path)?,
            AgentTarget::Windsurf => Self::install_windsurf(&exe)?,
            AgentTarget::Zcode => Self::install_zcode(&exe, custom_path)?,
            AgentTarget::Gemini => Self::install_gemini(&exe)?,
            AgentTarget::Qwen => Self::install_qwen(&exe)?,
            AgentTarget::Opencode => Self::install_opencode(&exe)?,
            AgentTarget::Hermes => {
                println!("💡 Для Hermes используйте 'ob2h install', 'ob2h plugin install' и 'ob2h skill install'");
            }
        }

        Ok(())
    }

    /// Проверка статуса установленных агентов.
    pub fn status() -> anyhow::Result<()> {
        let home = Self::home_dir();
        println!("📊 Статус подключения AI-агентов к OB2H:\n");

        let agents = [
            ("Claude Code", home.join(".claude").join("skills").join("ob2h").join("SKILL.md")),
            ("Cursor (global)", home.join(".cursor").join("mcp.json")),
            ("Windsurf / Cascade", home.join(".codeium").join("windsurf").join("mcp_config.json")),
            ("ZCode (global)", home.join(".zcode").join("mcp.json")),
            ("Gemini CLI / Antigravity", home.join(".gemini").join("antigravity-ide").join("mcp_config.json")),
            ("Qwen Code", home.join(".qwen").join("mcp.json")),
            ("OpenCode", home.join(".opencode").join("mcp.json")),
        ];

        for (name, path) in &agents {
            let status = if path.exists() { "✅ Подключен" } else { "❌ Не найден / не настроен" };
            println!("- {:<26} [{}] -> {}", name, status, path.display());
        }

        Ok(())
    }

    // --- Индивидуальные установщики ---

    pub fn install_claude(exe: &str) -> anyhow::Result<()> {
        let home = Self::home_dir();
        let skill_dir = home.join(".claude").join("skills").join("ob2h");
        fs::create_dir_all(&skill_dir)?;

        let skill_md = format!(
            r#"---
name: ob2h
description: "Долговременная память, AST-граф кода и дриминг для разработчика (OB2H)"
---

# OB2H Skill для Claude Code

Сервер OB2H предоставляет инструменты памяти (`memory_*`), графа кода (`project_*`, `graph_*`) и дриминга.

## Ключевые команды:
- `/ob2h scan` — сканирование кодовой базы проекта через AST (без расхода токенов)
- `/ob2h report` — дайджест архитектуры и выявление ключевых узлов (God Nodes)
- `/ob2h save <факт>` — сохранение важного архитектурного решения или факта
- `/ob2h search <запрос>` — гибридный поиск по памяти и графу

Исполняемый файл сервера: `{}`
"#,
            exe.replace('\\', "/")
        );

        let skill_file = skill_dir.join("SKILL.md");
        fs::write(&skill_file, skill_md)?;

        // Настройка mcp в ~/.claude/config.json
        let claude_cfg_dir = home.join(".claude");
        fs::create_dir_all(&claude_cfg_dir)?;
        let cfg_file = claude_cfg_dir.join("config.json");
        Self::upsert_mcp_json(&cfg_file, "ob2h", exe, &["serve"])?;

        println!("✅ Claude Code: скилл и MCP зарегистрированы в {}", skill_file.display());
        Ok(())
    }

    pub fn install_cursor(exe: &str, custom_path: Option<&str>) -> anyhow::Result<()> {
        let target_file = if let Some(p) = custom_path {
            PathBuf::from(p).join(".cursor").join("mcp.json")
        } else {
            Self::home_dir().join(".cursor").join("mcp.json")
        };

        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!("✅ Cursor: MCP зарегистрирован в {}", target_file.display());
        Ok(())
    }

    pub fn install_windsurf(exe: &str) -> anyhow::Result<()> {
        let target_file = Self::home_dir().join(".codeium").join("windsurf").join("mcp_config.json");
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!("✅ Windsurf: MCP зарегистрирован в {}", target_file.display());
        Ok(())
    }

    pub fn install_zcode(exe: &str, custom_path: Option<&str>) -> anyhow::Result<()> {
        let target_file = if let Some(p) = custom_path {
            PathBuf::from(p).join(".zcode").join("mcp.json")
        } else {
            Self::home_dir().join(".zcode").join("mcp.json")
        };

        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!("✅ ZCode: MCP зарегистрирован в {}", target_file.display());
        Ok(())
    }

    pub fn install_gemini(exe: &str) -> anyhow::Result<()> {
        let home = Self::home_dir();
        let target_file1 = home.join(".gemini").join("antigravity-ide").join("mcp_config.json");
        let target_file2 = home.join(".gemini").join("antigravity").join("mcp_config.json");

        if let Some(parent) = target_file1.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::upsert_mcp_json(&target_file1, "ob2h", exe, &["serve"])?;

        if let Some(parent) = target_file2.parent() {
            let _ = fs::create_dir_all(parent);
            let _ = Self::upsert_mcp_json(&target_file2, "ob2h", exe, &["serve"]);
        }

        println!("✅ Gemini / Antigravity: MCP зарегистрирован в {}", target_file1.display());
        Ok(())
    }

    pub fn install_qwen(exe: &str) -> anyhow::Result<()> {
        let target_file = Self::home_dir().join(".qwen").join("mcp.json");
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!("✅ Qwen Code: MCP зарегистрирован в {}", target_file.display());
        Ok(())
    }

    pub fn install_opencode(exe: &str) -> anyhow::Result<()> {
        let target_file = Self::home_dir().join(".opencode").join("mcp.json");
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!("✅ OpenCode: MCP зарегистрирован в {}", target_file.display());
        Ok(())
    }

    /// Вспомогательный метод для обновления JSON файла конфигурации MCP.
    pub fn upsert_mcp_json(file_path: &Path, server_name: &str, command: &str, args: &[&str]) -> anyhow::Result<()> {
        let mut json_val: serde_json::Value = if file_path.exists() {
            let content = fs::read_to_string(file_path).unwrap_or_else(|_| "{}".to_string());
            serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        if !json_val.is_object() {
            json_val = serde_json::json!({});
        }

        let mcp_servers = json_val
            .as_object_mut()
            .unwrap()
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));

        if let Some(obj) = mcp_servers.as_object_mut() {
            obj.insert(
                server_name.to_string(),
                serde_json::json!({
                    "command": command,
                    "args": args,
                    "env": {}
                }),
            );
        }

        let formatted = serde_json::to_string_pretty(&json_val)?;
        fs::write(file_path, formatted)?;
        Ok(())
    }
}
