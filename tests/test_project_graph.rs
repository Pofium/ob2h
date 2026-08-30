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
