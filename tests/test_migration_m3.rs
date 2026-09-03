//! Интеграционный тест миграции схемы M3 (Фаза 10).

use rusqlite::Connection;
use ob2h::db::schema::{migrate, schema_version, SCHEMA_VERSION};

#[test]
fn test_fresh_db_m3_migration() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;

    assert_eq!(schema_version(&conn), SCHEMA_VERSION);

    // Проверяем существование таблицы projects
    let project_count: i64 = conn.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))?;
    assert_eq!(project_count, 0);

    // Проверяем вставку проекта
    conn.execute(
        r#"
        INSERT INTO projects (id, name, root_path, description, tech_stack, created_at, updated_at)
        VALUES ('ob2h_test', 'OB2H Test', '/tmp/ob2h', 'Test Project', '["rust"]', datetime('now'), datetime('now'))
        "#,
        [],
    )?;

    let name: String = conn.query_row(
        "SELECT name FROM projects WHERE id = 'ob2h_test'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(name, "OB2H Test");

    // Проверяем новые колонки в memories
    conn.execute(
        r#"
        INSERT INTO memories (key, content, category, importance, source, project_id, created_at, updated_at)
        VALUES ('test_key', 'Memory in project', 'code', 0.8, 'manual', 'ob2h_test', datetime('now'), datetime('now'))
        "#,
        [],
    )?;

    let proj_id: Option<String> = conn.query_row(
        "SELECT project_id FROM memories WHERE key = 'test_key'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(proj_id, Some("ob2h_test".to_string()));

    // Проверяем новые колонки в graph_nodes и graph_edges
    conn.execute(
        r#"
        INSERT INTO graph_nodes (node_id, label, node_type, project_id, provenance, is_god_node, created_at, updated_at)
        VALUES ('n1', 'AuthService', 'Struct', 'ob2h_test', 'ast', 1, datetime('now'), datetime('now'))
        "#,
        [],
    )?;

    let (prov, is_god): (String, i64) = conn.query_row(
        "SELECT provenance, is_god_node FROM graph_nodes WHERE node_id = 'n1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(prov, "ast");
    assert_eq!(is_god, 1);

    Ok(())
}

#[test]
fn test_upgrade_from_m2_to_m3() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;

    // Накатываем M1 и M2 вручную
    conn.execute_batch(ob2h::db::schema::MIGRATION_V1)?;
    conn.execute_batch(ob2h::db::schema::MIGRATION_V2)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        [],
    )?;
    conn.execute(
        "INSERT INTO kv (key, value) VALUES ('schema_version', '2')",
        [],
    )?;

    // Создаем тестовую запись в M2
    conn.execute(
        r#"
        INSERT INTO memories (key, content, category, importance, source, created_at, updated_at)
        VALUES ('m2_key', 'Old fact before M3', 'general', 0.5, 'manual', datetime('now'), datetime('now'))
        "#,
        [],
    )?;

    // Запускаем migrate — должен автоматически применить M3 и последующие миграции
    migrate(&conn)?;

    assert_eq!(schema_version(&conn), SCHEMA_VERSION);

    // Проверяем, что старая запись на месте и project_id равен NULL
    let (content, proj_id): (String, Option<String>) = conn.query_row(
        "SELECT content, project_id FROM memories WHERE key = 'm2_key'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(content, "Old fact before M3");
    assert_eq!(proj_id, None);

    Ok(())
}
