use ob2h::db::Database;

#[test]
fn test_database_initialization_and_fts_trigram() {
    let db = Database::in_memory().expect("in memory db failed");

    // Проверка версии миграции
    let version = db.get_kv("schema_version").expect("get_kv").expect("version exists");
    assert_eq!(version, ob2h::db::schema::SCHEMA_VERSION.to_string());

    // Вставляем воспоминание на русском языке
    db.with_conn(|conn| {
        conn.execute(
            r#"
            INSERT INTO memories (key, content, category, importance, created_at, updated_at)
            VALUES ('m1', 'Иван работает над проектом OmnesBot в Москве', 'work', 0.8, '2026-08-18', '2026-08-18')
            "#,
            [],
        )?;
        Ok(())
    })
    .expect("insert memory");

    // Проверяем работу FTS5 с токенизатором trigram для русского языка
    let match_count: i64 = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'OmnesBot'",
                [],
                |row| row.get(0),
            )
        })
        .expect("fts query");
    assert_eq!(match_count, 1);

    let match_ru: i64 = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'Москве'",
                [],
                |row| row.get(0),
            )
        })
        .expect("fts ru query");
    assert_eq!(match_ru, 1);
}
