//! Интеграционные тесты уроков (v1.9.9, Фаза 64; миграция M11).

use ob2h::db::Database;
use ob2h::lifecycle::lessons::{fingerprint, LessonSaveRequest, LessonsService};

fn setup() -> anyhow::Result<(Database, LessonsService)> {
    let db = Database::in_memory()?;
    let svc = LessonsService::new(db.clone());
    Ok((db, svc))
}

fn save_req(
    content: &str,
    confidence: Option<f64>,
    project: Option<&str>,
    source_ids: Vec<String>,
) -> LessonSaveRequest {
    LessonSaveRequest {
        content: content.to_string(),
        context: Some("при работе с тестами".to_string()),
        confidence,
        project: project.map(String::from),
        tags: vec!["test".to_string()],
        source: Some("manual".to_string()),
        source_ids,
        actor: "test".to_string(),
        tool: "test".to_string(),
    }
}

#[test]
fn test_m11_tables_and_fts() -> anyhow::Result<()> {
    let (db, _svc) = setup()?;
    assert_eq!(
        db.with_conn(|c| Ok(ob2h::db::schema::schema_version(c)))?,
        ob2h::db::schema::SCHEMA_VERSION
    );

    // Таблицы существуют и принимают вставки
    db.with_conn(|c| {
        c.execute(
            "INSERT INTO lessons (id, content, created_at, updated_at) \
             VALUES ('lsn_x', 'всегда запускать cargo test перед коммитом', \
                     '2026-11-14T00:00:00Z', '2026-11-14T00:00:00Z')",
            [],
        )?;
        c.execute(
            "INSERT INTO slots (label, project, created_at, updated_at) \
             VALUES ('pending_items', '', '2026-11-14T00:00:00Z', '2026-11-14T00:00:00Z')",
            [],
        )?;
        c.execute(
            "INSERT INTO audit_log (ts, actor, action) VALUES ('2026-11-14T00:00:00Z', 'test', 'x')",
            [],
        )?;
        Ok(())
    })?;

    // FTS external content: триггер наполнил индекс, trigram находит
    let hits: i64 = db.with_conn(|c| {
        c.query_row(
            "SELECT COUNT(*) FROM lessons_fts WHERE lessons_fts MATCH 'cargo test'",
            [],
            |r| r.get(0),
        )
    })?;
    assert!(hits >= 1, "lessons_fts должен находить по FTS");
    Ok(())
}

#[test]
fn test_save_created_then_strengthened() -> anyhow::Result<()> {
    let (_db, svc) = setup()?;
    let content = "Всегда запускай cargo test перед коммитом";

    let out = svc.save(save_req(content, None, None, vec![]))?;
    assert_eq!(out.action, "created");
    assert_eq!(out.lesson.confidence, 0.5);
    assert_eq!(out.lesson.id, fingerprint(content));

    let out2 = svc.save(save_req(content, None, None, vec!["obs_1".to_string()]))?;
    assert_eq!(out2.action, "strengthened");
    // 0.5 + 0.1·(1−0.5) = 0.55
    assert!((out2.lesson.confidence - 0.55).abs() < 1e-9);
    assert_eq!(out2.lesson.reinforcements, 1);
    assert!(out2.lesson.source_ids.contains(&"obs_1".to_string()));

    let out3 = svc.save(save_req(
        content,
        None,
        None,
        vec!["obs_1".to_string(), "obs_2".to_string()],
    ))?;
    assert_eq!(out3.action, "strengthened");
    // 0.55 + 0.1·(1−0.55) = 0.595
    assert!((out3.lesson.confidence - 0.595).abs() < 1e-6);
    assert_eq!(out3.lesson.reinforcements, 2);
    // дедуп source_ids: obs_1 не задвоился
    assert_eq!(out3.lesson.source_ids.len(), 2);

    // Физически один урок
    let all = svc.list(None, None, 0.0, 100)?;
    assert_eq!(all.len(), 1);
    Ok(())
}

#[test]
fn test_recall_scoring_and_filters() -> anyhow::Result<()> {
    let (_db, svc) = setup()?;
    svc.save(save_req(
        "всегда прогоняй cargo fmt перед коммитом в репозитории ob2h",
        Some(0.9),
        Some("ob2h"),
        vec![],
    ))?;
    svc.save(save_req(
        "избегай deprecated API в модуле embedding сервиса",
        Some(0.3),
        Some("ob2h"),
        vec![],
    ))?;
    svc.save(save_req(
        "проверяй leases синхронизации перед push в проект self",
        Some(0.8),
        Some("self"),
        vec![],
    ))?;

    // Релевантность: cargo-урок выше
    let hits = svc.recall("cargo fmt", None, 0.1, 10, "test", "test")?;
    assert!(!hits.is_empty());
    assert!(hits[0].content.contains("cargo fmt"));
    assert!(hits[0].score.unwrap() > 0.0);

    // min_confidence отсекает слабый урок
    let hits = svc.recall("deprecated API", None, 0.5, 10, "test", "test")?;
    assert!(hits.iter().all(|l| l.confidence >= 0.5));

    // Проектный фильтр: урок другого проекта не попадает
    let hits = svc.recall("leases синхронизации", Some("ob2h"), 0.1, 10, "test", "test")?;
    assert!(hits.iter().all(|l| l.project.as_deref() == Some("ob2h")));

    // Tombstone скрыт из recall
    let all = svc.list(None, None, 0.0, 100)?;
    let victim = all[0].id.clone();
    assert!(svc.delete(&victim, "test", "test")?);
    let hits = svc.recall("cargo fmt", None, 0.1, 10, "test", "test")?;
    assert!(hits.iter().all(|l| l.id != victim));
    Ok(())
}

#[test]
fn test_delete_tombstone() -> anyhow::Result<()> {
    let (db, svc) = setup()?;
    let out = svc.save(save_req("не хардкодить пути C:\\ в Python-коде", None, None, vec![]))?;
    assert!(svc.delete(&out.lesson.id, "test", "test")?);
    assert!(svc.list(None, None, 0.0, 100)?.is_empty());
    // Повторное удаление — false
    assert!(!svc.delete(&out.lesson.id, "test", "test")?);
    // ADR-11: запись физически остаётся (tombstone), не удаляется
    let alive: i64 = db.with_conn(|c| {
        c.query_row(
            "SELECT COUNT(*) FROM lessons WHERE id = ?1 AND deleted_at IS NOT NULL",
            rusqlite::params![out.lesson.id],
            |r| r.get(0),
        )
    })?;
    assert_eq!(alive, 1);
    Ok(())
}

#[test]
fn test_decay_sweep_no_deletion_and_cursor() -> anyhow::Result<()> {
    let (db, svc) = setup()?;
    // Урок без укреплений, «спящий» ~19 недель → затухает до пола и становится
    // кандидатом, но НЕ удаляется (ADR-11)
    svc.save(save_req(
        "старый урок про настройку окружения который давно не подтверждается",
        None,
        None,
        vec![],
    ))?;
    svc.save(save_req("недавний укреплённый урок про nightly bench", None, None, vec![]))?;

    db.with_conn(|conn| {
        conn.execute(
            "UPDATE lessons SET created_at = '2026-07-01T00:00:00Z'",
            [],
        )?;
        Ok(())
    })?;

    let report = svc.decay_sweep("test")?;
    assert_eq!(report["swept"], true);
    let decayed = report["decayed"].as_u64().unwrap();
    assert!(decayed >= 1, "ожидали затухание, отчёт: {report}");

    // Записи живы (никаких автоудалений)
    let all = svc.list(None, None, 0.0, 100)?;
    assert_eq!(all.len(), 2);

    // Курсор: немедленный повтор — skip
    let again = svc.decay_sweep("test")?;
    assert_eq!(again["skipped"], true);
    Ok(())
}

#[test]
fn test_audit_log_records_operations() -> anyhow::Result<()> {
    let (db, svc) = setup()?;
    let out = svc.save(save_req("урок для аудита: сбрасывать кеш после миграций", None, None, vec![]))?;
    svc.recall("сбрасывать кеш", None, 0.1, 5, "test", "test")?;
    svc.delete(&out.lesson.id, "test", "test")?;

    let actions: Vec<String> = db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT action FROM audit_log WHERE action LIKE 'lesson_%'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    })?;
    for expected in ["lesson_save", "lesson_recall", "lesson_delete"] {
        assert!(
            actions.iter().any(|a| a == expected),
            "нет {expected} в audit_log: {actions:?}"
        );
    }
    Ok(())
}
