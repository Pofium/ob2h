//! Интеграционный тест честного инкрементального AST-сканирования и уважения .gitignore (Фаза 17).

use std::fs;
use tempfile::tempdir;
use ob2h::db::Database;
use ob2h::project::ProjectService;

#[test]
fn test_incremental_scan_and_gitignore() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let root = tmp.path();

    // 1. Создаём структуру репозитория с .gitignore
    fs::create_dir_all(root.join(".git"))?;
    let gitignore_content = "ignored_dir/\ntemp.rs\n*.log\n";
    fs::write(root.join(".gitignore"), gitignore_content)?;

    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir)?;

    let main_rs = r#"
    pub struct AppState {
        count: i32,
    }

    pub fn run_app() {
        println!("run");
    }
    "#;
    fs::write(src_dir.join("main.rs"), main_rs)?;

    let utils_rs = r#"
    pub fn helper_one() -> i32 {
        42
    }
    "#;
    fs::write(src_dir.join("utils.rs"), utils_rs)?;

    // Файлы, которые должны быть проигнорированы
    let ignored_dir = root.join("ignored_dir");
    fs::create_dir_all(&ignored_dir)?;
    fs::write(ignored_dir.join("secret.rs"), "pub fn secret() {}")?;
    fs::write(root.join("temp.rs"), "pub fn temp() {}")?;

    let db = Database::in_memory()?;
    let project_service = ProjectService::new(db.conn_arc());

    // Регистрируем проект
    let p = project_service.register_project(
        "inc_proj",
        "Incremental Project",
        root.to_str().unwrap(),
        Some("Test incremental scan with gitignore"),
        Some(&["rust".to_string()]),
    )?;
    assert_eq!(p.id, "inc_proj");

    // 2. Первичное сканирование (incremental = true)
    let res1 = project_service.scan_project("inc_proj", None, true)?;
    assert_eq!(res1.files_scanned, 2, "Должно быть просканировано ровно 2 файла (main.rs и utils.rs)");
    assert_eq!(res1.file_hashes.len(), 2);

    // Проверяем, что ignored_dir/secret.rs и temp.rs НЕ попали в граф
    let secret_node_count: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = 'inc_proj' AND label = 'secret'",
            [],
            |r| r.get(0),
        )
    })?;
    assert_eq!(secret_node_count, 0, "secret.rs должен игнорироваться по .gitignore");

    let temp_node_count: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = 'inc_proj' AND label = 'temp'",
            [],
            |r| r.get(0),
        )
    })?;
    assert_eq!(temp_node_count, 0, "temp.rs должен игнорироваться по .gitignore");

    // Проверяем наличие записей в таблице project_files
    let tracked_files: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM project_files WHERE project_id = 'inc_proj'",
            [],
            |r| r.get(0),
        )
    })?;
    assert_eq!(tracked_files, 2);

    // 3. Повторное сканирование БЕЗ изменений (incremental = true)
    let res2 = project_service.scan_project("inc_proj", None, true)?;
    assert_eq!(res2.files_scanned, 0, "При отсутствии изменений перепарсинг файлов не должен производиться");

    // 4. Модификация одного файла (utils.rs)
    let updated_utils_rs = r#"
    pub fn helper_one() -> i32 {
        42
    }

    pub fn helper_two() -> String {
        "hello".to_string()
    }
    "#;
    fs::write(src_dir.join("utils.rs"), updated_utils_rs)?;

    let res3 = project_service.scan_project("inc_proj", None, true)?;
    assert_eq!(res3.files_scanned, 1, "Должен быть перепарсен ровно 1 изменённый файл");

    // Проверяем, что в графе появился новый узел helper_two
    let helper_two_count: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = 'inc_proj' AND label = 'helper_two'",
            [],
            |r| r.get(0),
        )
    })?;
    assert_eq!(helper_two_count, 1, "Новый символ helper_two должен присутствовать в графе");

    // 5. Удаление файла (utils.rs)
    fs::remove_file(src_dir.join("utils.rs"))?;

    let res4 = project_service.scan_project("inc_proj", None, true)?;
    assert_eq!(res4.files_scanned, 0);

    // Проверяем, что узлы удалённого файла исчезли из графа
    let utils_nodes: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = 'inc_proj' AND file_path LIKE '%utils.rs%'",
            [],
            |r| r.get(0),
        )
    })?;
    assert_eq!(utils_nodes, 0, "Узлы удалённого файла должны быть вычищены из graph_nodes");

    // И запись из project_files удалена
    let tracked_after_delete: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM project_files WHERE project_id = 'inc_proj'",
            [],
            |r| r.get(0),
        )
    })?;
    assert_eq!(tracked_after_delete, 1, "В project_files должен остаться только 1 файл (main.rs)");

    Ok(())
}
