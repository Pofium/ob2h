//! Интеграционный тест сканирования проектов, выявления God Nodes и генерации отчетов (Фаза 12).

use std::fs;
use tempfile::tempdir;
use ob2h::db::Database;
use ob2h::graph::GraphAnalytics;
use ob2h::project::ProjectService;

#[test]
fn test_project_scan_and_analytics() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    let project_service = ProjectService::new(db.conn_arc());

    // Создаем временную директорию с кодом
    let tmp_dir = tempdir()?;
    let src_dir = tmp_dir.path().join("src");
    fs::create_dir_all(&src_dir)?;

    let main_rs = r#"
    use crate::auth::AuthService;
    use crate::db::DbPool;

    pub struct Server {
        auth: AuthService,
        db: DbPool,
    }

    pub fn main() {
        println!("start");
    }
    "#;

    let auth_rs = r#"
    pub struct AuthService;
    impl AuthService {
        pub fn login(&self) {}
    }
    "#;

    fs::write(src_dir.join("main.rs"), main_rs)?;
    fs::write(src_dir.join("auth.rs"), auth_rs)?;

    // 1. Регистрируем проект
    let p = project_service.register_project(
        "test_proj",
        "Test Project",
        tmp_dir.path().to_str().unwrap(),
        Some("A sample project for testing"),
        Some(&["rust".to_string()]),
    )?;
    assert_eq!(p.id, "test_proj");

    // 2. Сканируем проект
    let scan_res = project_service.scan_project("test_proj", None, false)?;
    assert!(scan_res.files_scanned >= 2);
    assert!(!scan_res.nodes.is_empty());
    assert!(!scan_res.edges.is_empty());

    // 3. Вычисляем God Nodes и генерируем отчет
    let report = db.with_conn(|conn| {
        GraphAnalytics::generate_project_report(conn, "test_proj")
    })?;

    assert_eq!(report.project_id, "test_proj");
    assert!(report.total_nodes > 0);
    assert!(!report.markdown_summary.is_empty());
    assert!(report.markdown_summary.contains("Test Project"));

    // 4. Проверяем генерацию <project_context>
    let ctx = db.with_conn(|conn| {
        GraphAnalytics::build_project_context(conn, "test_proj", Some("auth"))
    })?;

    assert!(ctx.contains("<project_context id=\"test_proj\">"));
    assert!(ctx.contains("</project_context>"));

    Ok(())
}

#[test]
fn test_detect_project_by_path_boundaries() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    let project_service = ProjectService::new(db.conn_arc());

    let tmp_root = tempdir()?;
    let app_dir = tmp_root.path().join("app");
    let app_test_dir = tmp_root.path().join("app-test");
    let nested_dir = app_dir.join("nested");
    fs::create_dir_all(&app_dir)?;
    fs::create_dir_all(&app_test_dir)?;
    fs::create_dir_all(&nested_dir)?;

    project_service.register_project("app", "App", app_dir.to_str().unwrap(), None, None)?;
    project_service.register_project("nested", "Nested", nested_dir.to_str().unwrap(), None, None)?;

    // 1. Точное совпадение
    let detected = project_service.detect_project_by_path(app_dir.to_str().unwrap())?;
    assert_eq!(detected.unwrap().id, "app");

    // 2. Вложенная директория внутри app
    let sub = app_dir.join("src");
    fs::create_dir_all(&sub)?;
    let detected_sub = project_service.detect_project_by_path(sub.to_str().unwrap())?;
    assert_eq!(detected_sub.unwrap().id, "app");

    // 3. Директория с похожим префиксом "app-test" НЕ должна матчиться на "app"
    let detected_diff = project_service.detect_project_by_path(app_test_dir.to_str().unwrap())?;
    assert!(detected_diff.is_none());

    // 4. Более глубокий (nested) проект должен иметь приоритет над родительским
    let detected_nested = project_service.detect_project_by_path(nested_dir.to_str().unwrap())?;
    assert_eq!(detected_nested.unwrap().id, "nested");

    Ok(())
}
