use anyhow::Result;
use ob2h::project::ProjectService;
use std::fs;

use tempfile::tempdir;

#[test]
fn test_incremental_crossfile_calls_preserved() -> Result<()> {
    let dir = tempdir()?;
    let db_path = dir.path().join("test.db");
    let proj_dir = dir.path().join("proj");
    let src_dir = proj_dir.join("src");
    fs::create_dir_all(&src_dir)?;

    // Создаем два связанных файла
    let file_a = src_dir.join("a.rs");
    let file_b = src_dir.join("b.rs");

    fs::write(
        &file_a,
        "use crate::b::helper;\n\npub fn run_app() {\n    helper();\n}\n",
    )?;
    fs::write(&file_b, "pub fn helper() -> i32 {\n    42\n}\n")?;

    let db = ob2h::db::Database::new(&db_path)?;
    let conn = db.conn_arc();
    let service = ProjectService::new(conn.clone());

    service.register_project("test_proj", "Test Project", &proj_dir.to_string_lossy(), None, None)?;

    // 1. Полное сканирование
    let scan1 = service.scan_project("test_proj", None, false)?;
    assert_eq!(scan1.files_scanned, 2);

    // Проверяем наличие ребра CALLS
    let calls_count: i64 = {
        let c = conn.lock().unwrap();
        c.query_row(
            "SELECT COUNT(*) FROM graph_edges WHERE label = 'CALLS' AND provenance = 'RESOLVED'",
            [],
            |r| r.get(0),
        )?
    };
    assert_eq!(calls_count, 1, "Ожидалось 1 CALLS ребро после полного скана");

    // 2. Модифицируем файл b.rs (добавляем новую функцию, helper остаётся)
    fs::write(
        &file_b,
        "pub fn helper() -> i32 {\n    42\n}\n\npub fn extra() {}\n",
    )?;

    // 3. Инкрементальное сканирование
    let scan2 = service.scan_project("test_proj", None, true)?;
    assert_eq!(scan2.files_scanned, 1);

    // Проверяем, что ребро CALLS из a.rs в b.rs не потеряно!
    let calls_count_after: i64 = {
        let c = conn.lock().unwrap();
        c.query_row(
            "SELECT COUNT(*) FROM graph_edges WHERE label = 'CALLS' AND provenance = 'RESOLVED'",
            [],
            |r| r.get(0),
        )?
    };
    assert_eq!(
        calls_count_after, 1,
        "CALLS ребро не должно исчезать после инкрементального скана!"
    );

    Ok(())
}
