//! Интеграционный тест File Watcher (Фаза 18).

use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::tempdir;
use ob2h::db::Database;
use ob2h::project::watcher::{is_code_file, ProjectWatcher};
use ob2h::project::ProjectService;

#[test]
fn test_is_code_file_filtering() {
    assert!(is_code_file(Path::new("src/main.rs")));
    assert!(is_code_file(Path::new("app/index.ts")));
    assert!(is_code_file(Path::new("scripts/test.py")));
    assert!(is_code_file(Path::new("db/schema.sql")));
    assert!(is_code_file(Path::new("lib/tool.go")));

    // Игнорируемые файлы и каталоги
    assert!(!is_code_file(Path::new("target/debug/app.exe")));
    assert!(!is_code_file(Path::new("target/debug/build.rs")));
    assert!(!is_code_file(Path::new(".git/HEAD")));
    assert!(!is_code_file(Path::new("node_modules/react/index.js")));
    assert!(!is_code_file(Path::new("dist/bundle.js")));
    assert!(!is_code_file(Path::new("README.md")));
    assert!(!is_code_file(Path::new("package.json")));
    assert!(!is_code_file(Path::new("Cargo.toml")));
}

#[tokio::test]
async fn test_project_watcher_switch_and_run() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let root = tmp.path();

    let src = root.join("src");
    fs::create_dir_all(&src)?;
    fs::write(src.join("main.rs"), "fn main() {}")?;

    let db = Database::in_memory()?;
    let project_service = Arc::new(ProjectService::new(db.conn_arc()));

    project_service.register_project(
        "watch_proj",
        "Watch Project",
        root.to_str().unwrap(),
        None,
        Some(&["rust".to_string()]),
    )?;

    let watcher = ProjectWatcher::new(project_service.clone(), 100);

    // Подключаем наблюдение за каталогом
    watcher.switch_project("watch_proj", root).await?;

    // Повторное переключение на тот же проект не должно вызывать ошибку
    watcher.switch_project("watch_proj", root).await?;

    Ok(())
}
