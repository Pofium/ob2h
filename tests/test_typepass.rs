//! Ф39 (трек C): type-resolve lite + provenance (M8).
//! Парсер-уровень: same-file / import / alias резолв вызовов; DB-уровень:
//! scan_project кладёт CALLS с provenance=RESOLVED, старые рёбра читаются.

use ob2h::db::Database;
use ob2h::project::ast::{AstCodeExtractor, AstScanResult};

fn parse_all(files: &[(&str, &str)]) -> AstScanResult {
    let ex = AstCodeExtractor::new();
    let mut out = AstScanResult::default();
    for (path, content) in files {
        ex.parse_file(path, content, &mut out);
    }
    out
}

#[test]
fn rust_calls_resolved_same_file_and_import() {
    let mut scan = parse_all(&[
        (
            "src/main.rs",
            r#"use crate::util::helper as h;

fn run(cfg: &Config) {
    let x = h(1);
    let y = helper(2);
    let z = unknown_thing(3);
}

fn helper(v: i32) -> i32 { v }
"#,
        ),
        (
            "src/util.rs",
            r#"pub fn helper(v: i32) -> i32 { v * 2 }
"#,
        ),
        (
            "src/config.rs",
            r#"pub struct Config;
"#,
        ),
    ]);
    ob2h::project::ast::resolve_calls(&mut scan);

    let calls: Vec<_> = scan.edges.iter().filter(|e| e.label == "CALLS").collect();
    assert!(!calls.is_empty(), "есть резолвнутые CALLS");
    for e in &calls {
        assert_eq!(
            e.provenance, "RESOLVED",
            "type-pass рёбра помечены RESOLVED"
        );
    }

    // same-file: run → helper (src/main.rs)
    assert!(
        calls
            .iter()
            .any(|e| e.source_node_id == "fn:src/main.rs:run"
                && e.target_node_id == "fn:src/main.rs:helper"),
        "same-file вызов резолвится: {:?}",
        calls
    );
    // import с алиасом: run → h(1) → helper (src/util.rs)
    assert!(
        calls
            .iter()
            .any(|e| e.source_node_id == "fn:src/main.rs:run"
                && e.target_node_id == "fn:src/util.rs:helper"
                && e.context.contains("import")),
        "алиас import резолвится: {:?}",
        calls
    );
    // unknown_thing — без ребра (AMBIGUOUS, не выдумываем)
    assert!(
        !calls
            .iter()
            .any(|e| e.source_node_id.contains("run") && e.context.contains("unknown")),
        "нерезолвленное не выдумывается"
    );
}

#[test]
fn python_from_import_resolved() {
    let scan = parse_all(&[
        (
            "app/main.py",
            "from .helpers import load as ld\n\ndef run():\n    data = ld(1)\n    return data\n",
        ),
        ("app/helpers.py", "def load(v):\n    return v\n"),
    ]);
    let mut scan = scan;
    ob2h::project::ast::resolve_calls(&mut scan);
    let calls: Vec<_> = scan.edges.iter().filter(|e| e.label == "CALLS").collect();
    assert!(
        calls
            .iter()
            .any(|e| e.source_node_id.contains("run") && e.target_node_id.contains("load")),
        "python from-import с алиасом резолвится: {:?}",
        calls
    );
    assert!(calls.iter().all(|e| e.provenance == "RESOLVED"));
}

#[test]
fn db_scan_stores_provenance_and_legacy_rows_readable() {
    let db = Database::in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/main.rs"),
        "use crate::util::helper;\n\nfn run() {\n    helper(1);\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/util.rs"),
        "pub fn helper(v: i32) -> i32 { v }\n",
    )
    .unwrap();

    let svc = ob2h::project::ProjectService::new(db.conn_arc());
    svc.register_project("tp", "tp", dir.path().to_str().unwrap(), None, None)
        .unwrap();
    let res = svc.scan_project("tp", None, false).unwrap();
    assert!(res.files_scanned >= 2);

    db.with_conn(|conn| {
        // CALLS-ребро run → helper записано с provenance=RESOLVED
        let prov: String = conn
            .query_row(
                "SELECT e.provenance FROM graph_edges e
                 JOIN graph_nodes a ON a.id = e.source_id
                 JOIN graph_nodes b ON b.id = e.target_id
                 WHERE e.label='CALLS' AND a.label='run' AND b.label='helper'",
                [],
                |r| r.get(0),
            )
            .expect("CALLS ребро в БД");
        assert_eq!(prov, "RESOLVED");

        // старое ребро с легаси-provenance читается как есть (M8-совместимость)
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, weight, contexts, created_at, project_id, provenance, updated_at)
             SELECT id, id, 'IMPORTS', 1.0, 'legacy', '2026-01-01T00:00:00Z', 'tp', 'ast', '2026-01-01T00:00:00Z'
             FROM graph_nodes WHERE label='run' LIMIT 1",
            [],
        )
        .unwrap();
        let legacy: String = conn
            .query_row(
                "SELECT provenance FROM graph_edges WHERE contexts='legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(legacy, "ast", "легаси-ребро читается со своим provenance");
        Ok::<(), rusqlite::Error>(())
    })
    .unwrap();
}

#[test]
fn rename_rescan_does_not_break_resolve() {
    let db = Database::in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/main.rs"),
        "fn run() {\n    helper(1);\n}\n\nfn helper(v: i32) -> i32 { v }\n",
    )
    .unwrap();

    let svc = ob2h::project::ProjectService::new(db.conn_arc());
    svc.register_project("rn", "rn", dir.path().to_str().unwrap(), None, None)
        .unwrap();
    svc.scan_project("rn", None, false).unwrap();

    // переименование helper → helper2
    std::fs::write(
        dir.path().join("src/main.rs"),
        "fn run() {\n    helper2(1);\n}\n\nfn helper2(v: i32) -> i32 { v }\n",
    )
    .unwrap();
    let res2 = svc.scan_project("rn", None, true).unwrap();
    assert!(res2.files_scanned >= 1, "рескан прошёл без паники");

    db.with_conn(|conn| {
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_edges e
                 JOIN graph_nodes a ON a.id = e.source_id
                 JOIN graph_nodes b ON b.id = e.target_id
                 WHERE e.label='CALLS' AND a.label='run' AND b.label='helper2' AND e.deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(cnt >= 1, "после переименования резолвится новое имя");
        Ok::<(), rusqlite::Error>(())
    })
    .unwrap();
}

/// Ф41: IMPORTS-рёбра резолвятся во внутренние файлы — относительные Python,
/// Vite-алиасы `@/…`, `as`/запятые нормализуются, внешнее остаётся модулем.
#[test]
fn imports_resolved_to_internal_files() {
    let mut scan = parse_all(&[
        (
            "backend/app/api/v1/router.py",
            "from .auth import router as auth_router\nimport os, sys\nimport numpy as np\n",
        ),
        ("backend/app/api/v1/auth.py", "def get_current_user():\n    return None\n"),
        (
            "frontend/src/renderer/pages/Chat.tsx",
            "import { cn } from '@/lib/utils';\nimport { x } from './local';\nimport React from 'react';\n",
        ),
        ("frontend/src/renderer/lib/utils.ts", "export const cn = () => '';\n"),
        ("frontend/src/renderer/pages/local.ts", "export const x = 1;\n"),
    ]);
    let n = ob2h::project::ast::resolve_imports(&mut scan);
    assert!(
        n >= 3,
        "как минимум 3 внутренних импорта резолвнуто, получено {n}"
    );

    let targets = |src: &str| -> Vec<String> {
        scan.edges
            .iter()
            .filter(|e| e.label == "IMPORTS" && e.source_node_id == src)
            .map(|e| e.target_node_id.clone())
            .collect()
    };

    let router = targets("file:backend/app/api/v1/router.py");
    assert!(
        router.contains(&"file:backend/app/api/v1/auth.py".to_string()),
        "относительный Python: {router:?}"
    );
    assert!(
        router.contains(&"module:os".to_string()) && router.contains(&"module:sys".to_string()),
        "`import os, sys` разворачивается и нормализуется: {router:?}"
    );
    assert!(
        router.contains(&"module:numpy".to_string()),
        "`import numpy as np` теряет алиас: {router:?}"
    );

    let chat = targets("file:frontend/src/renderer/pages/Chat.tsx");
    assert!(
        chat.contains(&"file:frontend/src/renderer/lib/utils.ts".to_string()),
        "Vite-алиас `@/…`: {chat:?}"
    );
    assert!(
        chat.contains(&"file:frontend/src/renderer/pages/local.ts".to_string()),
        "относительный `./local`: {chat:?}"
    );
    assert!(
        chat.contains(&"module:react".to_string()),
        "внешний пакет остаётся модулем: {chat:?}"
    );
}

/// Инкрементальный скан должен видеть цель импорта, даже если файл-цель не менялся.
#[test]
fn incremental_import_resolves_unchanged_target() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("app"))?;
    std::fs::write(dir.path().join("app/auth.py"), "def login():\n    pass\n")?;
    std::fs::write(dir.path().join("app/router.py"), "def route():\n    pass\n")?;

    let svc = ob2h::project::ProjectService::new(db.conn_arc());
    svc.register_project(
        "imports",
        "imports",
        dir.path().to_str().unwrap(),
        None,
        None,
    )?;
    svc.scan_project("imports", None, false)?;

    std::fs::write(
        dir.path().join("app/router.py"),
        "from .auth import login\n\ndef route():\n    login()\n",
    )?;
    let scan = svc.scan_project("imports", None, true)?;
    assert_eq!(scan.files_scanned, 1);

    let count: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_edges e
             JOIN graph_nodes src ON src.id = e.source_id
             JOIN graph_nodes dst ON dst.id = e.target_id
             WHERE e.project_id = 'imports' AND e.label = 'IMPORTS'
               AND src.node_id = 'file:app/router.py'
               AND dst.node_id = 'file:app/auth.py'",
            [],
            |row| row.get(0),
        )
    })?;
    assert_eq!(count, 1, "импорт должен указывать на неизменённый файл");
    Ok(())
}
