//! Интеграционный тест миграции M12 (Фазы 71.3/72b.2/72b.5/73.9, PLAN_v2.0):
//! словарь provenance v2.0, kind_detail/subtokens/bq_mask, confidence,
//! file_blobs, FTS с подтокенами.

use rusqlite::Connection;
use ob2h::db::schema::{migrate, schema_version, SCHEMA_VERSION};

/// Свежая БД: все колонки/таблицы v12 на месте, словарь не задевает пустые данные.
#[test]
fn test_fresh_db_m12_migration() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    assert_eq!(schema_version(&conn), SCHEMA_VERSION);

    // kind_detail / subtokens / bq_mask на graph_nodes
    let has_col = |name: &str, table: &str| -> anyhow::Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut it = stmt.query_map([], |r| r.get::<_, String>(1))?;
        Ok(it.any(|n| n.as_deref() == Ok(name)))
    };
    assert!(has_col("kind_detail", "graph_nodes")?);
    assert!(has_col("subtokens", "graph_nodes")?);
    assert!(has_col("bq_mask", "graph_nodes")?);
    assert!(has_col("bq_mask", "memories")?);
    // confidence уже была с M3 (REAL NOT NULL DEFAULT 1.0)
    assert!(has_col("confidence", "graph_edges")?);

    // file_blobs — структура 72b.5
    let cnt: i64 = conn.query_row("SELECT COUNT(*) FROM file_blobs", [], |r| r.get(0))?;
    assert_eq!(cnt, 0);

    // Вставка узла + подтокены через триггер
    conn.execute(
        "INSERT INTO graph_nodes (id, node_id, label, node_type, description, file_path, line_start, project_id, provenance, created_at, updated_at) \
         VALUES (1, 'fn:src/a.rs:fetch_user_token', 'fetch_user_token', 'Function', 'Функция', 'src/a.rs', 1, NULL, 'SYNTAX', datetime('now'), datetime('now'))",
        [],
    )?;
    // Триггер прокинул label/description при вставке (subtokens был '' на момент
    // вставки); после ручного UPDATE подтокены видны в таблице и FTS:
    conn.execute(
        "UPDATE graph_nodes SET subtokens = 'fetch user token' WHERE id = 1",
        [],
    )?;
    let hits: i64 = conn.query_row(
        "SELECT COUNT(*) FROM graph_nodes_fts WHERE graph_nodes_fts MATCH 'user'",
        [],
        |r| r.get(0),
    )?;
    assert!(hits >= 1, "FTS с подтокенами должен находить по под-токену");
    Ok(())
}

/// Путь v11 → v12: словарь provenance мигрируется, подтокены заполняются.
#[test]
fn test_v11_to_v12_vocabulary_and_subtokens() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    // Собираем БД уровня v11 вручную: полный batch, но version = 11
    for v in [
        ob2h::db::schema::MIGRATION_V1,
        ob2h::db::schema::MIGRATION_V2,
        ob2h::db::schema::MIGRATION_V3,
        ob2h::db::schema::MIGRATION_V4,
        ob2h::db::schema::MIGRATION_V5,
        ob2h::db::schema::MIGRATION_V6,
        ob2h::db::schema::MIGRATION_V7,
        ob2h::db::schema::MIGRATION_V8,
        ob2h::db::schema::MIGRATION_V9,
        ob2h::db::schema::MIGRATION_V10,
        ob2h::db::schema::MIGRATION_V11,
    ] {
        conn.execute_batch(v)?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO kv (key, value) VALUES ('schema_version', '11')",
        [],
    )?;

    // Узлы и рёбра со старым словарем v1.x
    conn.execute(
        "INSERT INTO graph_nodes (id, node_id, label, node_type, description, file_path, line_start, project_id, provenance, created_at, updated_at) \
         VALUES (1, 'fn:src/api.rs:run_server', 'run_server', 'Function', 'Функция run_server', 'src/api.rs', 1, NULL, 'EXTRACTED', datetime('now'), datetime('now'))",
        [],
    )?;
    conn.execute(
        "INSERT INTO graph_nodes (id, node_id, label, node_type, description, file_path, line_start, project_id, provenance, created_at, updated_at) \
         VALUES (2, 'route:GET /x', 'GET /x', 'Route', 'Маршрут', 'src/api.rs', 2, NULL, 'INFERRED', datetime('now'), datetime('now'))",
        [],
    )?;
    conn.execute(
        "INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at) \
         VALUES (1, 2, 'ROUTE', NULL, 'EXTRACTED', datetime('now'))",
        [],
    )?;

    migrate(&conn)?;
    assert_eq!(schema_version(&conn), SCHEMA_VERSION);

    // Словарь: EXTRACTED → SYNTAX, INFERRED → HEURISTIC
    let p1: String = conn.query_row("SELECT provenance FROM graph_nodes WHERE id = 1", [], |r| r.get(0))?;
    assert_eq!(p1, "SYNTAX");
    let p2: String = conn.query_row("SELECT provenance FROM graph_nodes WHERE id = 2", [], |r| r.get(0))?;
    assert_eq!(p2, "HEURISTIC");
    let pe: String = conn.query_row("SELECT provenance FROM graph_edges LIMIT 1", [], |r| r.get(0))?;
    assert_eq!(pe, "SYNTAX");

    // Подтокены заполнены Rust-проходом миграции (snake_case → части)
    let sub: String = conn.query_row("SELECT subtokens FROM graph_nodes WHERE id = 1", [], |r| r.get(0))?;
    assert_eq!(sub, "run server");

    // FTS: поиск по под-токену находит символ
    let hits: i64 = conn.query_row(
        "SELECT COUNT(*) FROM graph_nodes_fts WHERE graph_nodes_fts MATCH 'server'",
        [],
        |r| r.get(0),
    )?;
    assert!(hits >= 1, "FTS должен находить run_server по под-токену «server»");

    // confidence — M3-дефолт 1.0 (эвристики 71.x ставят <1.0 при записи)
    let conf: f64 = conn.query_row("SELECT confidence FROM graph_edges LIMIT 1", [], |r| r.get(0))?;
    assert!((conf - 1.0).abs() < 1e-9);

    // file_blobs вставляется и читается
    conn.execute(
        "INSERT INTO file_blobs (blake3_hash, rel_path, project_id, parser_version) \
         VALUES ('abc123', 'src/api.rs', 'p1', '45')",
        [],
    )?;
    let rp: String = conn.query_row("SELECT rel_path FROM file_blobs WHERE blake3_hash = 'abc123'", [], |r| r.get(0))?;
    assert_eq!(rp, "src/api.rs");

    Ok(())
}
