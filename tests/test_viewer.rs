//! Интеграционные тесты viewer (v1.9.9, Фазы 70–71).
//!
//! Главное правило Ф70.1: «ни одной записи» — сессия просмотра не меняет БД
//! (байты файла и WAL идентичны до/после). Секреты — единственное исключение
//! (§5.5), их тесты в src/viewer/secrets_ctl.rs. GUI не требуется: всё ядро
//! headless, окно — тонкая оболочка.

use std::path::Path;

use ob2h::db::Database;
use ob2h::viewer::{data, lock, secrets_ctl};

/// Снимок байтов БД + WAL (для сравнения «до/после»).
fn db_snapshot(db_path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for suffix in ["", "-wal"] {
        let p = std::path::PathBuf::from(format!("{}{}", db_path.display(), suffix));
        if p.exists() {
            out.push((suffix.to_string(), std::fs::read(&p).unwrap()));
        }
    }
    out
}

/// Наполненная БД: память, граф, уроки, слоты, daily-лог.
/// Писатель (Database) возвращается и держится живым в тестах — реалистичный
/// сценарий: serve держит БД, пока работает viewer. Без живого писателя SQLite
/// пересоздаёт WAL/-shm и правит служебный заголовок при открытии/закрытии
/// соединения (не записи данных) — байтовое сравнение было бы нестабильным.
fn seeded() -> (tempfile::TempDir, std::path::PathBuf, Database) {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("ob2h.db");
    let db = Database::new(&db_path).unwrap();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, source, trust, project_id, meta, created_at, updated_at) \
             VALUES ('k1', 'первая запись про rust', 'general', 0.9, 'manual', 0.7, 'ob2h', '{\"a\":1}', '2026-11-01T10:00:00Z', '2026-11-01T10:00:00Z')",
            [],
        )?;
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, source, trust, created_at, updated_at, deleted_at) \
             VALUES ('k2', 'удалённая запись', 'general', 0.5, 'chat', 0.5, '2026-11-01', '2026-11-01', '2026-11-02')",
            [],
        )?;
        for i in 0..6 {
            conn.execute(
                "INSERT INTO graph_nodes (node_id, label, node_type, description, created_at, updated_at) \
                 VALUES (?1, ?2, 'Technology', 'описание', '2026-11-01', '2026-11-01')",
                rusqlite::params![format!("n{i}"), format!("Node {i}")],
            )?;
        }
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, created_at) VALUES (1, 2, 'RELATED', '2026-11-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO lessons (id, content, context, confidence, source, created_at, updated_at) \
             VALUES ('lsn_test', 'урок про тесты', 'контекст', 0.8, 'manual', '2026-11-01', '2026-11-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO lessons (id, content, confidence, source, created_at, updated_at, deleted_at) \
             VALUES ('lsn_dead', 'удалённый урок', 0.5, 'manual', '2026-11-01', '2026-11-01', '2026-11-02')",
            [],
        )?;
        conn.execute(
            "INSERT INTO slots (label, project, content, size_limit, created_at, updated_at) \
             VALUES ('ctx', '', 'содержимое слота', 2000, '2026-11-01', '2026-11-01')",
            [],
        )?;
        // Чекпоинт: WAL пуст, снимок байтов стабилен.
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    })
    .unwrap();

    // Daily-лог для таба «Сессии».
    let daily = tmp.path().join("workspace").join("daily");
    std::fs::create_dir_all(&daily).unwrap();
    std::fs::write(
        daily.join("2026-11-13.jsonl"),
        "{\"timestamp\":\"2026-11-13T10:00:00Z\",\"user_text\":\"привет\",\"assistant_text\":\"здравствуйте\",\"meta\":{\"session_id\":\"s1\",\"author\":\"hermes\"}}\n\
         битая строка пропускается\n",
    )
    .unwrap();
    (tmp, db_path, db)
}

/// Ф70.1 DoD: полная сессия просмотра (все табы) не меняет ни байта БД.
#[test]
fn viewing_session_writes_nothing() {
    let (tmp, db_path, _db) = seeded();
    let before = db_snapshot(&db_path);

    // «Сессия просмотра»: открытие + запросы всех табов + повторные обновления.
    {
        let conn = data::open_readonly(&db_path).unwrap();
        for _ in 0..3 {
            let f = data::MemoryFilters { query: "rust", ..Default::default() };
            let rows = data::search_memories(&conn, &f).unwrap();
            assert_eq!(rows.len(), 1, "tombstone не должен попадать в выдачу");
            let _ = data::memory_links(&conn, rows[0].id).unwrap();
            let _ = data::distinct_memory_values(&conn, "category").unwrap();
            let _ = data::load_graph(&conn, 300, &data::GraphScope::All).unwrap();
            let _ = data::load_lessons(&conn, 100).unwrap();
            let _ = data::load_slots(&conn).unwrap();
            let _ = data::load_stats(&conn, &db_path).unwrap();
            let _ = data::load_sessions(tmp.path());
        }
    }

    let after = db_snapshot(&db_path);
    assert_eq!(before, after, "просмотр не должен менять БД");
    assert!(!tmp.path().join("viewer.lock").exists());
}

/// Ф70.1: query_only запрещает запись даже через fallback-соединение.
#[test]
fn readonly_connection_is_query_only() {
    let (_tmp, db_path, _db) = seeded();
    let conn = data::open_readonly(&db_path).unwrap();
    assert!(conn.execute("UPDATE memories SET content = 'x' WHERE id = 1", []).is_err());
    assert!(conn.execute("DELETE FROM memories", []).is_err());
    assert!(conn.execute_batch("CREATE TABLE hack (x TEXT);").is_err());
}

/// Ф70.4: сессии читаются из daily, битые строки пропускаются, каталоги не создаются.
#[test]
fn sessions_load_without_creating_dirs() {
    let (tmp, _db_path, _db) = seeded();
    let entries = data::load_sessions(tmp.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, "s1");
    assert_eq!(entries[0].author.as_deref(), Some("hermes"));
    assert!(entries[0].source_ref.contains("daily/2026-11-13.jsonl#L1"));

    // Пустой data_dir: нет каталога workspace — viewer не создаёт его.
    let empty = tempfile::tempdir().unwrap();
    assert!(data::load_sessions(empty.path()).is_empty());
    assert!(!empty.path().join("workspace").exists());
}

/// Ф70.5: tombstones уроков скрыты, слоты видны.
#[test]
fn lifecycle_hides_tombstones() {
    let (_tmp, db_path, _db) = seeded();
    let conn = data::open_readonly(&db_path).unwrap();
    let lessons = data::load_lessons(&conn, 100).unwrap();
    assert_eq!(lessons.len(), 1);
    assert_eq!(lessons[0].id, "lsn_test");
    let slots = data::load_slots(&conn).unwrap();
    assert_eq!(slots.len(), 1);
    assert!(slots[0].project.is_empty(), "global-слот");
}

/// Ф70.1: single-instance lockfile — второй старт отклоняется, мёртвый pid перехватывается.
#[test]
fn lockfile_single_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let _lock = lock::ViewerLock::acquire(tmp.path()).unwrap();
    let err = lock::ViewerLock::acquire(tmp.path()).unwrap_err().to_string();
    assert!(err.contains("[Error]") && err.contains("уже открыто"), "{err}");
    assert!(lock::ViewerLock::is_running(tmp.path()));
}

/// Ф71.4/Ф72.1: headless-сборка честно сообщает об отсутствии gui.
#[test]
#[cfg(not(feature = "gui"))]
fn headless_is_honest() {
    let tmp = tempfile::tempdir().unwrap();
    let err = ob2h::viewer::run(tmp.path(), 1800).unwrap_err().to_string();
    assert!(err.contains("[Error]"), "{err}");
    assert!(err.contains("[Подсказка]"), "{err}");
    assert!(err.contains("gui"), "{err}");
    assert_eq!(
        ob2h::viewer::open_detached(tmp.path()).unwrap(),
        ob2h::viewer::OpenStatus::GuiUnavailable
    );
}

/// Ф71: разблокировка viewer независима от serve — свой SecretsService на своём
/// соединении; после «закрытия окна» (shutdown) ключ затёрт.
#[test]
fn secrets_unlock_independent_and_zeroized_on_close() {
    let (_tmp, db_path, _db) = seeded();
    let ro = data::open_readonly(&db_path).unwrap();
    let mut ctl = secrets_ctl::SecretsController::new(&db_path, 1800);
    assert_eq!(ctl.state(&ro), secrets_ctl::SecretsState::NotInitialized);
    ctl.init("4321", "4321").unwrap();
    ctl.unlock("4321").unwrap();
    ctl.set("token", "value-123", None).unwrap();
    assert_eq!(ctl.reveal("token").unwrap().as_str(), "value-123");

    // «Закрытие окна».
    ctl.shutdown();
    assert_eq!(ctl.state(&ro), secrets_ctl::SecretsState::Locked);
    assert!(ctl.reveal("token").is_err(), "ключ должен быть затёрт");

    // Значение не утекло в аудит.
    let db = Database::new(&db_path).unwrap();
    db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT details FROM audit_log")?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for d in rows {
            assert!(!d.contains("value-123"), "значение в аудите: {d}");
        }
        Ok(())
    })
    .unwrap();
    // Просмотр остальных табов после операций с секретами по-прежнему read-only.
    let _ = data::load_stats(&ro, &db_path).unwrap();
}