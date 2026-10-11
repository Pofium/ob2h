//! Интеграционный тест DSH-интеграции (`ob2h agent install --agent dsh`).
//!
//! Тесты вызывают чистые функции с явными путями — окружение процесса не меняется,
//! поэтому файл безопасен при параллельном запуске.

use ob2h::cli::agent::{AgentManager, DshPatchOutcome};
use std::fs;
use tempfile::tempdir;

const USER_PATCH: &str = "- id: ui-theme\n  name: '@deepseek-ai/dsh-client-ui-theme'\n  config:\n    preference: dark\n";

#[test]
fn test_patch_block_fields() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let exe = r"C:\Projects\ob2h\target\release\ob2h.exe";
    let block = AgentManager::render_dsh_patch_block(
        exe,
        &std::path::PathBuf::from(r"C:\Projects\ob2h"),
        &std::path::PathBuf::from(r"C:\Projects\ob2h\data"),
        None,
    );

    // Поля Config, которые требует dsh-mcp-client.
    assert!(block.contains("@deepseek-ai/dsh-mcp-client"));
    assert!(block.contains("serverName: ob2h"));
    assert!(block.contains("transport: stdio"));
    assert!(block.contains("command: 'C:/Projects/ob2h/target/release/ob2h.exe'"));
    assert!(block.contains("args: ['serve']"));
    assert!(block.contains("cwd: 'C:/Projects/ob2h'"));
    assert!(block.contains("OB2H_DATA_DIR: 'C:/Projects/ob2h/data'"));
    // Секрет в патче не хранится вообще: ключ ob2h берёт из .env рядом с cwd.
    // Упоминание переменной в поясняющем комментарии допустимо — важны отсутствие
    // YAML-строки присваивания и отсутствие подстановки пустой строки.
    assert!(!block.contains("OB2H_LLM_API_KEY:"));
    assert!(!block.contains("!!js"));
    assert!(!block.contains("sk-"));
    // Без исходника плагина строка insert остаётся одна (MCP-клиент) + пояснение.
    assert_eq!(block.matches("- insert:").count(), 1);
    assert!(!block.contains("id: ob2h-memory"));
    assert!(block.contains("cordis-плагин-провайдер не найден"));
    // Обратные слэши в YAML-одинарных кавычках ломали бы путь на Linux-сборке.
    assert!(!block.contains(r"C:\"));
    let _ = tmp;
    Ok(())
}

#[test]
fn test_patch_block_with_plugin_provider() -> anyhow::Result<()> {
    let block = AgentManager::render_dsh_patch_block(
        r"C:\Projects\ob2h\target\release\ob2h.exe",
        &std::path::PathBuf::from(r"C:\Projects\ob2h"),
        &std::path::PathBuf::from(r"C:\Projects\ob2h\data"),
        Some(&std::path::PathBuf::from(
            r"C:\Projects\ob2h\plugin\dsh-ob2h\index.js",
        )),
    );

    // Второй insert — cordis-плагин-провайдер с абсолютным путём (0.2.0-rc.2 его резолвит).
    assert_eq!(block.matches("- insert:").count(), 1);
    assert!(block.contains("    - id: ob2h-memory"));
    assert!(block.contains("name: 'C:/Projects/ob2h/plugin/dsh-ob2h/index.js'"));
    assert!(block.contains("binary: 'C:/Projects/ob2h/target/release/ob2h.exe'"));
    assert!(block.contains("source: 'dsh'"));
    assert!(block.contains("author: 'dsh'"));
    assert!(!block.contains("{{"));
    assert!(!block.contains(r"C:\"));
    Ok(())
}

#[test]
fn test_plugin_entry_and_llm_key_detection() -> anyhow::Result<()> {
    // В тестовом окружении cwd — корень репозитория, исходник плагина на месте.
    let cwd = std::env::current_dir()?;
    let entry = AgentManager::dsh_plugin_entry("ob2h", &cwd);
    assert!(
        entry
            .as_ref()
            .map(|p| p.ends_with("index.js"))
            .unwrap_or(false),
        "плагин должен находиться от корня репозитория: {entry:?}"
    );

    let tmp = tempdir()?;
    assert!(!AgentManager::llm_key_configured(tmp.path()));
    fs::write(tmp.path().join(".env"), "OB2H_LLM_API_KEY=\n")?;
    assert!(!AgentManager::llm_key_configured(tmp.path()));
    fs::write(
        tmp.path().join(".env"),
        "OB2H_LLM_BASE_URL=https://api.deepseek.com/v1\nOB2H_LLM_API_KEY=test-key-value\n",
    )?;
    assert!(AgentManager::llm_key_configured(tmp.path()));
    Ok(())
}

#[test]
fn test_upsert_appends_and_preserves_user_patches() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let patch = tmp.path().join("cordis.patch.yml");
    fs::write(&patch, USER_PATCH)?;

    let block = AgentManager::render_dsh_patch_block(
        "C:/ob2h/ob2h.exe",
        &std::path::PathBuf::from("C:/ob2h"),
        &std::path::PathBuf::from("C:/ob2h/data"),
        None,
    );
    let outcome = AgentManager::upsert_dsh_patch(&patch, &block)?;
    assert_eq!(outcome, DshPatchOutcome::Appended);

    let content = fs::read_to_string(&patch)?;
    assert!(content.starts_with("- id: ui-theme"));
    assert!(content.contains("!!js") || content.contains("preference: dark"));
    assert_eq!(content.matches("serverName: ob2h").count(), 1);
    assert!(AgentManager::dsh_patch_configured(&patch));
    Ok(())
}

#[test]
fn test_upsert_creates_missing_file() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let patch = tmp.path().join("nested").join("cordis.patch.yml");
    let block = AgentManager::render_dsh_patch_block(
        "C:/ob2h/ob2h.exe",
        &std::path::PathBuf::from("C:/ob2h"),
        &std::path::PathBuf::from("C:/ob2h/data"),
        None,
    );

    let outcome = AgentManager::upsert_dsh_patch(&patch, &block)?;
    assert_eq!(outcome, DshPatchOutcome::Created);
    assert!(AgentManager::dsh_patch_configured(&patch));
    Ok(())
}

#[test]
fn test_upsert_is_idempotent_and_updatable() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let patch = tmp.path().join("cordis.patch.yml");
    fs::write(&patch, USER_PATCH)?;

    let block_old = AgentManager::render_dsh_patch_block(
        "C:/old/ob2h.exe",
        &std::path::PathBuf::from("C:/old"),
        &std::path::PathBuf::from("C:/old/data"),
        None,
    );
    assert_eq!(
        AgentManager::upsert_dsh_patch(&patch, &block_old)?,
        DshPatchOutcome::Appended
    );
    let after_first = fs::read_to_string(&patch)?;

    // Повторная установка ничего не меняет — побайтово.
    assert_eq!(
        AgentManager::upsert_dsh_patch(&patch, &block_old)?,
        DshPatchOutcome::Unchanged
    );
    assert_eq!(fs::read_to_string(&patch)?, after_first);

    // Переустановка после переезда бинарника обновляет блок, не размножая его.
    let block_new = AgentManager::render_dsh_patch_block(
        "C:/new/ob2h.exe",
        &std::path::PathBuf::from("C:/new"),
        &std::path::PathBuf::from("C:/new/data"),
        None,
    );
    assert_eq!(
        AgentManager::upsert_dsh_patch(&patch, &block_new)?,
        DshPatchOutcome::Updated
    );
    let content = fs::read_to_string(&patch)?;
    assert!(content.contains("C:/new/ob2h.exe"));
    assert!(!content.contains("C:/old/ob2h.exe"));
    assert_eq!(content.matches("serverName: ob2h").count(), 1);
    assert_eq!(content.matches("- insert:").count(), 1);
    assert!(content.starts_with("- id: ui-theme"));
    Ok(())
}

#[test]
fn test_upsert_skips_manual_entry() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let patch = tmp.path().join("cordis.patch.yml");
    let manual = "- insert:\n    - id: my-ob2h\n      name: '@deepseek-ai/dsh-mcp-client'\n      config:\n        serverName: ob2h\n";
    fs::write(&patch, manual)?;

    let block = AgentManager::render_dsh_patch_block(
        "C:/ob2h/ob2h.exe",
        &std::path::PathBuf::from("C:/ob2h"),
        &std::path::PathBuf::from("C:/ob2h/data"),
        None,
    );
    assert_eq!(
        AgentManager::upsert_dsh_patch(&patch, &block)?,
        DshPatchOutcome::ManualEntryFound
    );
    assert_eq!(fs::read_to_string(&patch)?, manual);
    // Ручная запись тоже считается настроенным подключением.
    assert!(AgentManager::dsh_patch_configured(&patch));
    Ok(())
}

#[test]
fn test_skill_render_has_frontmatter_and_paths() {
    let skill = AgentManager::render_dsh_skill(
        "C:/ob2h/ob2h.exe",
        &std::path::PathBuf::from("C:/ob2h/data"),
        &std::path::PathBuf::from("C:/ob2h"),
        &std::path::PathBuf::from("C:/Users/u/.dsh"),
    );

    // DSH-провайдер скиллов игнорирует файл без name/description в kebab-case.
    assert!(skill.starts_with("---\n"));
    assert!(skill.contains("\nname: ob2h\n"));
    assert!(skill.contains("description:"));
    // Все плейсхолдеры подставлены.
    assert!(!skill.contains("{{"));
    assert!(skill.contains("C:/ob2h/ob2h.exe"));
    assert!(skill.contains("C:/Users/u/.dsh"));
}
