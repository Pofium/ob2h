//! Тесты задач work-list по ob2h:
//!  * сгенерированный protobuf (`*_pb2.py`, `*_pb2_grpc.py`) не сканируется;
//!  * `project scan --full` (incremental=false) пересобирает узлы даже без
//!    изменения файлов;
//!  * смена версии AST-экстрактора автоматически форсирует полный рескан.

use std::fs;

use ob2h::db::Database;
use ob2h::project::ast::AstCodeExtractor;
use ob2h::project::{ProjectService, AST_EXTRACTOR_VERSION};
use tempfile::tempdir;

#[test]
fn generated_pb2_files_are_not_scanned() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let root = tmp.path();
    fs::write(root.join("handwritten.py"), "def real_fn():\n    return 1\n")?;
    fs::write(root.join("arks_pb2.py"), "def generated_fn():\n    return 1\n")?;
    fs::write(root.join("thing_pb2_grpc.py"), "def grpc_fn():\n    return 1\n")?;

    let extractor = AstCodeExtractor::new();
    let res = extractor.scan_directory(root, None);

    let files: Vec<&String> = res.file_hashes.keys().collect();
    assert!(files.iter().any(|f| f.ends_with("handwritten.py")));
    assert!(
        files.iter().all(|f| !f.contains("_pb2")),
        "сгенерированный protobuf не должен сканироваться: {files:?}"
    );
    assert!(res.nodes.iter().all(|n| n.label != "generated_fn" && n.label != "grpc_fn"));
    Ok(())
}

fn setup_project(id: &str) -> anyhow::Result<(tempfile::TempDir, Database, ProjectService)> {
    let tmp = tempdir()?;
    let root = tmp.path();
    fs::create_dir_all(root.join("pkg"))?;
    fs::write(root.join("pkg/mod.py"), "def foo():\n    return 1\n")?;

    let db = Database::in_memory()?;
    let svc = ProjectService::new(db.conn_arc());
    svc.register_project(id, "T", root.to_str().unwrap(), None, Some(&["python".to_string()]))?;
    Ok((tmp, db, svc))
}

fn label_count(db: &Database, id: &str, label: &str) -> i64 {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = ?1 AND label = ?2",
            rusqlite::params![id, label],
            |r| r.get(0),
        )
    })
    .expect("count")
}

fn stale_the_node(db: &Database, id: &str) {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE graph_nodes SET label='STALE' WHERE project_id = ?1 AND label='foo'",
            rusqlite::params![id],
        )?;
        Ok(())
    })
    .expect("stale");
}

#[test]
fn full_scan_rebuilds_nodes_without_file_changes() -> anyhow::Result<()> {
    let (_tmp, db, svc) = setup_project("fullp")?;
    svc.scan_project("fullp", None, false)?;
    assert_eq!(label_count(&db, "fullp", "foo"), 1);

    stale_the_node(&db, "fullp");
    assert_eq!(label_count(&db, "fullp", "foo"), 0);

    // `--full` → incremental=false: узел пересобирается из неизменённого файла
    svc.scan_project("fullp", None, false)?;
    assert_eq!(label_count(&db, "fullp", "foo"), 1);
    assert_eq!(label_count(&db, "fullp", "STALE"), 0);
    Ok(())
}

#[test]
fn extractor_version_bump_forces_full_rescan() -> anyhow::Result<()> {
    let (_tmp, db, svc) = setup_project("verp")?;

    // Первый инкрементальный скан фиксирует текущую версию экстрактора
    svc.scan_project("verp", None, true)?;
    let v: String = db.with_conn(|conn| {
        conn.query_row(
            "SELECT value FROM kv WHERE key = 'ast_extractor_version:verp'",
            [],
            |r| r.get(0),
        )
    })?;
    assert_eq!(v, AST_EXTRACTOR_VERSION);

    // Узел «устарел» и версия в kv откатывается
    stale_the_node(&db, "verp");
    db.set_kv("ast_extractor_version:verp", "1")?;

    // Скан запрошен инкрементальный, но версия не совпала → авто-полный
    svc.scan_project("verp", None, true)?;
    assert_eq!(label_count(&db, "verp", "foo"), 1);
    assert_eq!(label_count(&db, "verp", "STALE"), 0);
    Ok(())
}
