//! Мультиагентный менеджер интеграций (Фаза 14).
//! Установка и настройка OB2H для: Claude Code, Cursor, Windsurf, ZCode, Gemini CLI / Antigravity, Qwen Code, OpenCode,
//! DSH (DeepSeek Harness), Hermes.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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
    Dsh,
    Hermes,
}

/// Маркеры автоуправляемого блока OB2H в `cordis.patch.yml` профиля DSH.
const DSH_PATCH_BEGIN: &str = "# >>> ob2h:begin";
const DSH_PATCH_END: &str = "# <<< ob2h:end";

/// Блок patch-слоя профиля DSH: два `insert` — MCP-клиент (инструменты) и
/// cordis-плагин-провайдер (автозахват ходов + prefetch памяти).
/// Плейсхолдеры: `{{BINARY}}`, `{{PROJECT_DIR}}`, `{{DATA_DIR}}`, `{{LLM_BASE_URL}}`,
/// `{{LLM_MODEL}}`, `{{PLUGIN_ITEM}}` (готовый элемент списка `insert` под плагин).
const DSH_PATCH_BLOCK_TEMPLATE: &str = r#"# >>> ob2h:begin — блок управляется `ob2h agent install --agent dsh`; ручные правки теряются
- insert:
    - id: mcp-ob2h
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: ob2h
        transport: stdio
        command: '{{BINARY}}'
        args: ['serve']
        cwd: '{{PROJECT_DIR}}'
        toolCallTimeoutMs: 300000
        env:
          OB2H_DATA_DIR: '{{DATA_DIR}}'
          OB2H_LLM_BASE_URL: '{{LLM_BASE_URL}}'
          OB2H_LLM_MODEL: '{{LLM_MODEL}}'
          # API-ключ берётся самим ob2h из .env рядом с cwd (в .gitignore) или из
          # OB2H_LLM_API_KEY в окружении хоста; пустую строку сюда НЕ пишем — она
          # затирает переменную и мешает ob2h подхватить .env.
{{PLUGIN_ITEM}}# <<< ob2h:end — конец блока ob2h"#;

/// Исходник скилла для DSH (`skills/dsh/SKILL.md`), ставится в `$DSH_HOME/skills/ob2h/`.
const DSH_SKILL_SOURCE: &str = include_str!("../../skills/dsh/SKILL.md");

/// Что произошло при записи блока в patch-файл профиля DSH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DshPatchOutcome {
    /// Файла не было — создан.
    Created,
    /// Блок ob2h заменён на актуальный.
    Updated,
    /// Блок добавлен в конец существующего файла.
    Appended,
    /// Блок уже актуален, запись не потребовалась.
    Unchanged,
    /// В файле есть ручная запись `serverName: ob2h` — не трогаем.
    ManualEntryFound,
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

                if Self::install_claude(&exe).is_ok() {
                    installed += 1;
                }
                if Self::install_cursor(&exe, custom_path).is_ok() {
                    installed += 1;
                }
                if Self::install_windsurf(&exe).is_ok() {
                    installed += 1;
                }
                if Self::install_zcode(&exe, custom_path).is_ok() {
                    installed += 1;
                }
                if Self::install_gemini(&exe).is_ok() {
                    installed += 1;
                }
                if Self::install_qwen(&exe).is_ok() {
                    installed += 1;
                }
                if Self::install_opencode(&exe).is_ok() {
                    installed += 1;
                }
                if Self::dsh_profile_dir(&Self::dsh_home()).is_some()
                    && Self::install_dsh(&exe, None).is_ok()
                {
                    installed += 1;
                }

                println!("✨ Готово! Настроено агентов: {}", installed);
            }
            AgentTarget::Claude => Self::install_claude(&exe)?,
            AgentTarget::Cursor => Self::install_cursor(&exe, custom_path)?,
            AgentTarget::Windsurf => Self::install_windsurf(&exe)?,
            AgentTarget::Zcode => Self::install_zcode(&exe, custom_path)?,
            AgentTarget::Gemini => Self::install_gemini(&exe)?,
            AgentTarget::Qwen => Self::install_qwen(&exe)?,
            AgentTarget::Opencode => Self::install_opencode(&exe)?,
            AgentTarget::Dsh => Self::install_dsh(&exe, custom_path)?,
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
            (
                "Claude Code",
                home.join(".claude")
                    .join("skills")
                    .join("ob2h")
                    .join("SKILL.md"),
            ),
            ("Cursor (global)", home.join(".cursor").join("mcp.json")),
            (
                "Windsurf / Cascade",
                home.join(".codeium")
                    .join("windsurf")
                    .join("mcp_config.json"),
            ),
            ("ZCode (global)", home.join(".zcode").join("mcp.json")),
            (
                "Gemini CLI / Antigravity",
                home.join(".gemini")
                    .join("antigravity-ide")
                    .join("mcp_config.json"),
            ),
            ("Qwen Code", home.join(".qwen").join("mcp.json")),
            ("OpenCode", home.join(".opencode").join("mcp.json")),
        ];

        for (name, path) in &agents {
            let status = if path.exists() {
                "✅ Подключен"
            } else {
                "❌ Не найден / не настроен"
            };
            println!("- {:<26} [{}] -> {}", name, status, path.display());
        }

        // DSH: подключение живёт не в отдельном файле, а блоком в patch-слое профиля.
        let dsh_home = Self::dsh_home();
        let dsh_name = "DSH (DeepSeek Harness)";
        let (dsh_status, dsh_path) = match Self::dsh_profile_dir(&dsh_home) {
            Some(profile) => {
                let patch = profile.join("cordis.patch.yml");
                let status = if Self::dsh_patch_configured(&patch) {
                    "✅ Подключен"
                } else {
                    "❌ Не найден / не настроен"
                };
                (status, patch)
            }
            None => ("❌ Не найден / не настроен", dsh_home.join("profiles")),
        };
        println!(
            "- {:<26} [{}] -> {}",
            dsh_name,
            dsh_status,
            dsh_path.display()
        );

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

        println!(
            "✅ Claude Code: скилл и MCP зарегистрированы в {}",
            skill_file.display()
        );
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
        let target_file = Self::home_dir()
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json");
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!(
            "✅ Windsurf: MCP зарегистрирован в {}",
            target_file.display()
        );
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
        let target_file1 = home
            .join(".gemini")
            .join("antigravity-ide")
            .join("mcp_config.json");
        let target_file2 = home
            .join(".gemini")
            .join("antigravity")
            .join("mcp_config.json");

        if let Some(parent) = target_file1.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::upsert_mcp_json(&target_file1, "ob2h", exe, &["serve"])?;

        if let Some(parent) = target_file2.parent() {
            let _ = fs::create_dir_all(parent);
            let _ = Self::upsert_mcp_json(&target_file2, "ob2h", exe, &["serve"]);
        }

        println!(
            "✅ Gemini / Antigravity: MCP зарегистрирован в {}",
            target_file1.display()
        );
        Ok(())
    }

    pub fn install_qwen(exe: &str) -> anyhow::Result<()> {
        let target_file = Self::home_dir().join(".qwen").join("mcp.json");
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!(
            "✅ Qwen Code: MCP зарегистрирован в {}",
            target_file.display()
        );
        Ok(())
    }

    pub fn install_opencode(exe: &str) -> anyhow::Result<()> {
        let target_file = Self::home_dir().join(".opencode").join("mcp.json");
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        Self::upsert_mcp_json(&target_file, "ob2h", exe, &["serve"])?;
        println!(
            "✅ OpenCode: MCP зарегистрирован в {}",
            target_file.display()
        );
        Ok(())
    }

    // --- DSH (DeepSeek Harness) ---

    /// Дом DSH: `$DSH_HOME` или `~/.dsh`.
    pub fn dsh_home() -> PathBuf {
        match std::env::var("DSH_HOME") {
            Ok(h) if !h.is_empty() => PathBuf::from(h),
            _ => Self::home_dir().join(".dsh"),
        }
    }

    /// Каталог активного профиля DSH: `$DSH_PROFILE`, иначе desktop/web, иначе первый по алфавиту.
    pub fn dsh_profile_dir(home: &Path) -> Option<PathBuf> {
        let profiles = home.join("profiles");
        if let Ok(name) = std::env::var("DSH_PROFILE") {
            if !name.is_empty() {
                let dir = profiles.join(name);
                return dir.is_dir().then_some(dir);
            }
        }
        for name in ["desktop", "web"] {
            let dir = profiles.join(name);
            if dir.is_dir() {
                return Some(dir);
            }
        }
        let mut dirs: Vec<PathBuf> = fs::read_dir(&profiles)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs.into_iter().next()
    }

    /// Настроен ли ob2h в patch-слое профиля (наш блок или ручная запись `serverName: ob2h`).
    pub fn dsh_patch_configured(patch_file: &Path) -> bool {
        fs::read_to_string(patch_file)
            .map(|c| c.contains(DSH_PATCH_BEGIN) || c.contains("serverName: ob2h"))
            .unwrap_or(false)
    }

    /// Блок `insert` для `cordis.patch.yml` — ровно поля Config плагина `dsh-mcp-client`
    /// плюс, если найден исходник, элемент под cordis-плагин-провайдер `plugin/dsh-ob2h`.
    /// Ключ LLM в патч не пишется: ob2h сам берёт его из `.env` рядом с cwd или из env хоста.
    pub fn render_dsh_patch_block(
        exe: &str,
        project_dir: &Path,
        data_dir: &Path,
        plugin_entry: Option<&Path>,
    ) -> String {
        let sl = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let base_url = std::env::var("OB2H_LLM_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/v1".to_string());
        let model = std::env::var("OB2H_LLM_MODEL")
            .unwrap_or_else(|_| "deepseek-v4-flash".to_string());
        let plugin_item = match plugin_entry {
            Some(path) => format!(
                "    - id: ob2h-memory\n      name: '{}'\n      config:\n        binary: '{}'\n        cwd: '{}'\n        dataDir: '{}'\n        source: 'dsh'\n        author: 'dsh'\n",
                sl(path),
                exe.replace('\\', "/"),
                sl(project_dir),
                sl(data_dir)
            ),
            None => "    # cordis-плагин-провайдер не найден (plugin/dsh-ob2h/index.js) — работает только MCP-клиент\n".to_string(),
        };
        DSH_PATCH_BLOCK_TEMPLATE
            .replace("{{BINARY}}", &exe.replace('\\', "/"))
            .replace("{{PROJECT_DIR}}", &sl(project_dir))
            .replace("{{DATA_DIR}}", &sl(data_dir))
            .replace("{{LLM_BASE_URL}}", &base_url)
            .replace("{{LLM_MODEL}}", &model)
            .replace("{{PLUGIN_ITEM}}", &plugin_item)
    }

    /// Найти корень репозитория ob2h по расположению бинарника или рабочему каталогу.
    pub fn ob2h_repo_root(exe: &str, project_dir: &Path) -> Option<PathBuf> {
        let exe_dir = Path::new(exe).parent()?;
        // target/release/ob2h.exe → корень репозитория на два уровня выше.
        [exe_dir.join("../.."), project_dir.to_path_buf()]
            .into_iter()
            .find(|candidate| candidate.join("Cargo.toml").is_file())
    }

    /// Путь к входному файлу cordis-плагина `dsh-ob2h` (если исходник на месте).
    pub fn dsh_plugin_entry(exe: &str, project_dir: &Path) -> Option<PathBuf> {
        let relative = Path::new("plugin").join("dsh-ob2h").join("index.js");
        let mut candidates = vec![project_dir.join(&relative)];
        if let Some(root) = Self::ob2h_repo_root(exe, project_dir) {
            candidates.insert(0, root.join(&relative));
        }
        candidates.into_iter().find(|p| p.is_file())
    }

    /// Идемпотентная вставка/обновление блока по маркерам; остальные патчи не трогаются.
    pub fn upsert_dsh_patch(
        patch_file: &Path,
        block: &str,
    ) -> anyhow::Result<DshPatchOutcome> {
        let existed = patch_file.exists();
        let existing = if existed {
            fs::read_to_string(patch_file)?
        } else {
            String::new()
        };
        let block = block.trim_end();

        let begin = existing.find(DSH_PATCH_BEGIN);
        let end = existing.find(DSH_PATCH_END);
        if let (Some(b), Some(e)) = (begin, end) {
            if e > b {
                // Маркер — только префикс строки-комментария, поэтому заменяем строку целиком.
                let tail_from = existing[e..]
                    .find('\n')
                    .map(|i| e + i)
                    .unwrap_or(existing.len());
                let mut next = String::with_capacity(existing.len() + block.len());
                next.push_str(&existing[..b]);
                next.push_str(block);
                next.push_str(&existing[tail_from..]);
                if next == existing {
                    return Ok(DshPatchOutcome::Unchanged);
                }
                fs::write(patch_file, next)?;
                return Ok(DshPatchOutcome::Updated);
            }
        }

        // Ручную регистрацию ob2h не дублируем: две записи с одним serverName не загрузятся.
        if existing.contains("serverName: ob2h") {
            return Ok(DshPatchOutcome::ManualEntryFound);
        }

        if let Some(parent) = patch_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut next = existing;
        if !next.is_empty() {
            if !next.ends_with('\n') {
                next.push('\n');
            }
            next.push('\n');
        }
        next.push_str(block);
        next.push('\n');
        fs::write(patch_file, next)?;
        Ok(if existed {
            DshPatchOutcome::Appended
        } else {
            DshPatchOutcome::Created
        })
    }

    /// SKILL.md для DSH (корень скиллов `$DSH_HOME/skills`) с путями этой машины.
    pub fn render_dsh_skill(
        exe: &str,
        data_dir: &Path,
        project_dir: &Path,
        dsh_home: &Path,
    ) -> String {
        DSH_SKILL_SOURCE
            .replace("{{BINARY}}", exe)
            .replace("{{DATA_DIR}}", &data_dir.to_string_lossy())
            .replace("{{PROJECT_DIR}}", &project_dir.to_string_lossy())
            .replace("{{DSH_HOME}}", &dsh_home.to_string_lossy())
    }

    /// Интеграция DSH: MCP-сервер ob2h через `dsh-mcp-client` + скилл `ob2h`.
    /// `custom_path` — каталог профиля DSH (если autodetect не подходит).
    pub fn install_dsh(exe: &str, custom_path: Option<&str>) -> anyhow::Result<()> {
        let dsh_home = Self::dsh_home();
        let project_dir = std::env::current_dir()?;
        let data_dir = match std::env::var("OB2H_DATA_DIR") {
            Ok(d) if !d.is_empty() => PathBuf::from(d),
            _ => project_dir.join("data"),
        };

        let profile_dir = match custom_path {
            Some(p) => {
                let dir = PathBuf::from(p);
                fs::create_dir_all(&dir)?;
                dir
            }
            None => Self::dsh_profile_dir(&dsh_home).ok_or_else(|| {
                anyhow::anyhow!(
                    "профиль DSH не найден в {}: задайте DSH_HOME/DSH_PROFILE или --path",
                    dsh_home.join("profiles").display()
                )
            })?,
        };
        let profile_name = profile_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| profile_dir.display().to_string());

        let patch_file = profile_dir.join("cordis.patch.yml");
        let plugin_entry = Self::dsh_plugin_entry(exe, &project_dir);
        let block =
            Self::render_dsh_patch_block(exe, &project_dir, &data_dir, plugin_entry.as_deref());
        match Self::upsert_dsh_patch(&patch_file, &block)? {
            DshPatchOutcome::Unchanged => println!(
                "✅ DSH (профиль {}): блок ob2h в {} уже актуален",
                profile_name,
                patch_file.display()
            ),
            DshPatchOutcome::ManualEntryFound => println!(
                "⚠️ DSH (профиль {}): в {} уже есть ручная запись `serverName: ob2h` — блок не добавлен",
                profile_name,
                patch_file.display()
            ),
            _ => println!(
                "✅ DSH (профиль {}): MCP-сервер ob2h зарегистрирован в {}",
                profile_name,
                patch_file.display()
            ),
        }

        let skill_file = dsh_home.join("skills").join("ob2h").join("SKILL.md");
        if let Some(parent) = skill_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &skill_file,
            Self::render_dsh_skill(exe, &data_dir, &project_dir, &dsh_home),
        )?;
        println!("✅ DSH: скилл ob2h установлен: {}", skill_file.display());

        println!("💡 Профили с patchReload: live подхватывают патч без перезапуска; иначе перезапустите DSH.");
        match &plugin_entry {
            Some(path) => println!(
                "✅ DSH: cordis-плагин памяти подключён (автозахват ходов + prefetch <agent_memory>): {}",
                path.display()
            ),
            None => println!(
                "⚠️ DSH: cordis-плагин не найден (ожидался plugin/dsh-ob2h/index.js рядом с Cargo.toml) — работает только MCP-клиент, автозахвата ходов не будет"
            ),
        }
        println!("💡 Инструменты появятся как mcp__ob2h__*.");
        if !Self::llm_key_configured(&project_dir) {
            println!(
                "⚠️ LLM-инструменты (dream_run, knowledge_extract, graph_reason) выключены: положите OB2H_LLM_API_KEY в {}",
                project_dir.join(".env").display()
            );
        }
        Ok(())
    }

    /// Есть ли ключ LLM: в окружении процесса или в `.env` рабочего каталога ob2h.
    pub fn llm_key_configured(project_dir: &Path) -> bool {
        if std::env::var("OB2H_LLM_API_KEY")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            return true;
        }
        fs::read_to_string(project_dir.join(".env"))
            .map(|content| {
                content.lines().any(|line| {
                    line.trim_start()
                        .strip_prefix("OB2H_LLM_API_KEY=")
                        .map(|value| !value.trim().trim_matches('"').trim_matches('\'').is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }

    /// Вспомогательный метод для обновления JSON файла конфигурации MCP.
    pub fn upsert_mcp_json(
        file_path: &Path,
        server_name: &str,
        command: &str,
        args: &[&str],
    ) -> anyhow::Result<()> {
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
