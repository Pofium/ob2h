//! Ф38 (трек C): edit-time blast radius — hint по последнему изменённому
//! символу + top-5 callers, TTL 30 мин, warn-only (OB2H_EDIT_BLAST=warn).

use ob2h::db::Database;
use ob2h::graph::blast::{self, BLAST_TTL_SECS};

const PROJ: &str = "blast";

/// Фикстура: hub-функция с тремя callers + правка (updated_at = сейчас).
fn seed_graph(db: &Database) {
    let now = chrono::Utc::now().to_rfc3339();
    db.with_conn(|conn| {
        conn.execute_batch(&format!(
            "INSERT INTO projects (id, name, root_path, created_at, updated_at)
             VALUES ('blast', 'blast', '/tmp/blast', '{now}', '{now}');

             INSERT INTO graph_nodes (node_id, label, node_type, project_id, file_path, description, is_god_node, created_at, updated_at)
             VALUES
               ('s:hub',  'run_pipeline', 'Function', 'blast', 'src/sync/mod.rs', '', 0,
                '{now}', '{now}'),
               ('s:c1', 'apply_inbox', 'Function', 'blast', 'src/sync/mod.rs', '', 0,
                '{now}', '{now}'),
               ('s:c2', 'export_opts',  'Function', 'blast', 'src/sync/mod.rs', '', 0,
                '{now}', '{now}'),
               ('s:c3', 'import_file',  'Function', 'blast', 'src/sync/mod.rs', '', 0,
                '{now}', '{now}'),
               ('s:old', 'legacy_fn',   'Function', 'blast', 'src/old.rs', '', 0,
                '2026-09-13T10:00:00Z', '2020-01-01T00:00:00Z');

             INSERT INTO graph_edges (source_id, target_id, label, weight, project_id, provenance, created_at)
             SELECT c.id, h.id, 'CALLS', 1.0, 'blast', 'ast_scan', '{now}'
             FROM graph_nodes c, graph_nodes h
             WHERE h.label='run_pipeline' AND c.label IN ('apply_inbox','export_opts','import_file');
             "
        ))
    })
    .unwrap();
}

#[test]
fn fresh_edit_produces_hint_with_callers() {
    let db = Database::in_memory().unwrap();
    seed_graph(&db);
    let block = db
        .with_conn(|conn| Ok::<String, rusqlite::Error>(blast::warn_block(conn, Some(PROJ), true)))
        .unwrap();
    assert!(
        block.starts_with("[blast-radius] правка `run_pipeline`"),
        "блок: {block}"
    );
    assert!(block.contains("apply_inbox"));
    assert!(block.contains("export_opts"));
    assert!(block.contains("import_file"));
    assert!(
        !block.contains("legacy_fn"),
        "старый символ не должен быть hint'ом"
    );

    // hint закэширован в kv с TTL ≈ 30 мин
    let hint = db
        .with_conn(|conn| Ok::<_, rusqlite::Error>(blast::latest_hint(conn, PROJ).unwrap()))
        .unwrap()
        .expect("kv hint");
    assert_eq!(hint.symbol, "run_pipeline");
    assert_eq!(hint.callers.len(), 3);
    assert!(hint.expires_at > hint.updated_at);
    assert!(hint.expires_at - hint.updated_at <= BLAST_TTL_SECS);
}

#[test]
fn expired_hint_and_disabled_flag_are_silent() {
    let db = Database::in_memory().unwrap();
    seed_graph(&db);

    // Флаг off (дефолт) — блока нет, даже при свежей правке
    let block = db
        .with_conn(|conn| Ok::<String, rusqlite::Error>(blast::warn_block(conn, Some(PROJ), false)))
        .unwrap();
    assert!(block.is_empty(), "флаг off: пусто, получено: {block}");

    // Нет активного проекта — пусто
    let block = db
        .with_conn(|conn| Ok::<String, rusqlite::Error>(blast::warn_block(conn, None, true)))
        .unwrap();
    assert!(block.is_empty());

    // Протухший kv-hint перекрывается... его нет -> compute_hint тоже молчит
    // (legacy_fn старый, run_pipeline свежий — но проверим TTL-граничный случай:
    // удалим свежие updated_at, оставив только старый символ)
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE graph_nodes SET updated_at = '2020-01-01T00:00:00Z' WHERE label != 'legacy_fn'",
            [],
        )
    })
    .unwrap();
    let block = db
        .with_conn(|conn| Ok::<String, rusqlite::Error>(blast::warn_block(conn, Some(PROJ), true)))
        .unwrap();
    assert!(
        block.is_empty(),
        "все правки старше TTL: пусто, получено: {block}"
    );
}

#[test]
fn no_active_project_or_no_changes_is_fail_open() {
    let db = Database::in_memory().unwrap();
    // пустая БД без таблиц projects/graph_nodes? — in_memory создаёт схему:
    // отсутствие строк не должно паниковать
    let block = db
        .with_conn(|conn| {
            Ok::<String, rusqlite::Error>(blast::warn_block(conn, Some("nope"), true))
        })
        .unwrap();
    assert!(block.is_empty());
}
