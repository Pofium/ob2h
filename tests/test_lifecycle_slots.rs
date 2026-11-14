//! Тесты слотов рабочего контекста (v1.9.9, Фаза 65): затенение, лимиты,
//! read_only, seed-дефолты, бюджет инъекции, sync roundtrip с LWW-конфликтом.

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::init_app;
use ob2h::lifecycle::slots::SlotRecord;
use ob2h::mcp::AppContext;
use std::sync::Arc;
use tempfile::tempdir;

fn svc() -> ob2h::lifecycle::SlotsService {
    let db = Database::in_memory().expect("in-memory db");
    ob2h::lifecycle::SlotsService::new(db)
}

#[test]
fn test_label_validation() {
    assert!(ob2h::lifecycle::slots::validate_label("pending_items").is_ok());
    assert!(ob2h::lifecycle::slots::validate_label("Bad").is_err());
    assert!(ob2h::lifecycle::slots::validate_label("1abc").is_err());
    assert!(ob2h::lifecycle::slots::validate_label("has space").is_err());
    assert!(ob2h::lifecycle::slots::validate_label("").is_err());
}

#[test]
fn test_create_get_shadowing() -> anyhow::Result<()> {
    let svc = svc();
    let g = svc.create(
        "notes", "глобальная заметка", None, None, true, "global", None, "test", "test",
    )?;
    assert_eq!(g.scope, "global");
    let p = svc.create(
        "notes", "проектная заметка", None, None, true, "project", Some("ob2h"), "test", "test",
    )?;
    assert_eq!(p.scope, "project");

    // Затенение: в контексте проекта — project-слот; без проекта — global
    assert_eq!(svc.get("notes", Some("ob2h"))?.content, "проектная заметка");
    assert_eq!(svc.get("notes", None)?.content, "глобальная заметка");

    // Дубликат в своём скоупе запрещён
    assert!(svc
        .create("notes", "x", None, None, true, "project", Some("ob2h"), "test", "test")
        .is_err());

    // В list — один слот на label (project затеняет)
    let list = svc.list(Some("ob2h"))?;
    let notes: Vec<&SlotRecord> = list.iter().filter(|s| s.label == "notes").collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].scope, "project");
    Ok(())
}

#[test]
fn test_append_overflow_413_and_replace() -> anyhow::Result<()> {
    let svc = svc();
    svc.create(
        "tiny", "abc", Some(10), None, true, "project", Some("p"), "test", "test",
    )?;
    // 413-семантика: отказ с размерами и подсказкой
    let err = svc
        .append("tiny", Some("p"), "0123456789ABCDEF", "test", "test")
        .unwrap_err()
        .to_string();
    assert!(err.contains("sizeLimit"), "{err}");
    assert!(err.contains("slot_replace"), "{err}");

    // Компактизация через replace, после неё append влезает
    // (append склеивает построчно — семантика agentmemory)
    svc.replace("tiny", Some("p"), "xy", "test", "test")?;
    let s = svc.append("tiny", Some("p"), "z", "test", "test")?;
    assert_eq!(s.content, "xy\nz");
    Ok(())
}

#[test]
fn test_read_only_protection() -> anyhow::Result<()> {
    let (db, svc) = {
        let db = Database::in_memory()?;
        (db.clone(), ob2h::lifecycle::SlotsService::new(db))
    };
    svc.create("locked", "важно", None, None, true, "global", None, "test", "test")?;
    db.with_conn(|c| {
        c.execute("UPDATE slots SET read_only = 1 WHERE label = 'locked'", [])?;
        Ok(())
    })?;
    assert!(svc.append("locked", None, "x", "test", "test").is_err());
    assert!(svc
        .replace("locked", None, "x", "test", "test")
        .is_err());
    assert!(svc.delete("locked", None, "test", "test").is_err());
    // Содержимое не тронуто
    assert_eq!(svc.get("locked", None)?.content, "важно");
    Ok(())
}

#[test]
fn test_seed_defaults_and_audit() -> anyhow::Result<()> {
    let (db, svc) = {
        let db = Database::in_memory()?;
        (db.clone(), ob2h::lifecycle::SlotsService::new(db))
    };
    svc.ensure_defaults("ob2h")?;
    svc.ensure_defaults("ob2h")?; // идемпотентно
    let list = svc.list(Some("ob2h"))?;
    let labels: Vec<&str> = list.iter().map(|s| s.label.as_str()).collect();
    for expected in [
        "pending_items",
        "session_guidance",
        "project_context",
        "user_rules",
        "self_notes",
    ] {
        assert!(labels.contains(&expected), "нет {expected}: {labels:?}");
    }
    // pending_items — pinned; self_notes — не pinned (§5.3)
    let pending = list.iter().find(|s| s.label == "pending_items").unwrap();
    assert!(pending.pinned);
    let notes = list.iter().find(|s| s.label == "self_notes").unwrap();
    assert!(!notes.pinned);

    // Аудит: операции слотов пишутся в audit_log (Ф68-таблица из M11)
    svc.create(
        "audit_probe", "", None, None, true, "project", Some("ob2h"), "test", "test",
    )?;
    svc.append("audit_probe", Some("ob2h"), "текст", "test", "test")?;
    let actions: Vec<String> = db.with_conn(|c| {
        let mut stmt = c.prepare("SELECT action FROM audit_log WHERE action LIKE 'slot_%'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    })?;
    for expected in ["slot_create", "slot_append"] {
        assert!(actions.contains(&expected.to_string()), "{actions:?}");
    }
    Ok(())
}

#[test]
fn test_pinned_context_budget_and_shadowing() -> anyhow::Result<()> {
    let (db, svc) = {
        let db = Database::in_memory()?;
        (db.clone(), ob2h::lifecycle::SlotsService::new(db))
    };
    svc.create(
        "focus", "фокус-слот проекта", None, None, true, "project", Some("p"), "test", "test",
    )?;
    svc.create(
        "focus", "глобальный фокус", None, None, true, "global", None, "test", "test",
    )?;
    let big_content = "x".repeat(800);
    svc.create(
        "big", &big_content, None, None, true, "project", Some("p"), "test", "test",
    )?;
    svc.create(
        "hidden", "не инъектируется", None, None, false, "project", Some("p"), "test", "test",
    )?;
    svc.create("empty", "   ", None, None, true, "project", Some("p"), "test", "test")?;

    let ctx = || {
        db.with_conn(|c| Ok(ob2h::lifecycle::slots::pinned_context(c, Some("p"))))
            .unwrap()
    };

    // Затенение: в контексте — project-версия focus
    let block = ctx();
    assert!(block.contains("<agent_slots>"), "{block}");
    assert!(block.contains("фокус-слот проекта"));
    assert!(!block.contains("глобальный фокус"));
    // pinned=false и пустой контент не попадают
    assert!(!block.contains("не инъектируется"));
    assert!(!block.contains("## empty"));

    // Бюджет: OB2H_SLOTS_MAX_CHARS меньше содержимого big → big отрезан целиком
    std::env::set_var("OB2H_SLOTS_MAX_CHARS", "80");
    let small = ctx();
    std::env::remove_var("OB2H_SLOTS_MAX_CHARS");
    assert!(!small.contains("## big"), "{small}");
    assert!(small.contains("фокус-слот проекта"));

    // Бюджет 0 — блок пуст
    std::env::set_var("OB2H_SLOTS_MAX_CHARS", "0");
    assert!(ctx().is_empty());
    std::env::remove_var("OB2H_SLOTS_MAX_CHARS");
    Ok(())
}

fn make_ctx(dir: &std::path::Path, origin: &str, priority: &[&str]) -> Arc<AppContext> {
    let sync_dir = dir.join("sync");
    std::fs::create_dir_all(&sync_dir).unwrap();
    let peers = if priority.is_empty() {
        format!("{{\"origin\": \"{origin}\"}}\n")
    } else {
        format!(
            "{{\"origin\": \"{origin}\", \"priority\": [{}]}}\n",
            priority.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ")
        )
    };
    std::fs::write(sync_dir.join("peers.json"), peers).unwrap();
    let mut settings = Settings::from_env();
    settings.data_dir = dir.to_path_buf();
    settings.embed_provider = "fake".to_string();
    init_app(settings).expect("init_app")
}

#[tokio::test]
async fn test_sync_roundtrip_slots_and_lessons() -> anyhow::Result<()> {
    let tmp_a = tempdir()?;
    let tmp_b = tempdir()?;
    let a = make_ctx(tmp_a.path(), "alpha", &[]);
    let b = make_ctx(tmp_b.path(), "beta", &[]);

    // Слот и урок на A → full-экспорт → импорт на B
    a.slots.create(
        "roundtrip", "v1 с alpha", Some(500), None, true, "project", Some("proj"), "test", "test",
    )?;
    let lesson = a.lessons.save(ob2h::lifecycle::LessonSaveRequest {
        content: "учить sync-урок тестово".into(),
        context: None,
        confidence: Some(0.7),
        project: Some("proj".into()),
        tags: vec![],
        source: Some("manual".into()),
        source_ids: vec![],
        actor: "test".into(),
        tool: "test".into(),
    })?;

    let bundle = a.sync.export_opts("default", true)?;
    let stats = b.sync.import_file(&bundle).await?;
    assert!(stats.slots_applied >= 1, "slots_applied=0: {stats:?}");
    assert!(stats.lessons_applied >= 1, "lessons_applied=0: {stats:?}");
    assert_eq!(
        b.slots.get("roundtrip", Some("proj"))?.content,
        "v1 с alpha"
    );
    let got_lesson = b
        .lessons
        .list(Some("proj"), None, 0.0, 50)?
        .into_iter()
        .find(|l| l.id == lesson.lesson.id)
        .expect("урок на B");
    assert!((got_lesson.confidence - 0.7).abs() < 1e-9);

    // Правка на B с более новым updated_at → LWW отдаёт её A при обратном импорте
    b.slots.append("roundtrip", Some("proj"), "+ beta", "test", "test")?;
    b.db.with_conn(|c| {
        c.execute(
            "UPDATE slots SET updated_at = '2026-12-01T00:00:00Z' WHERE label = 'roundtrip'",
            [],
        )?;
        Ok(())
    })?;
    let bundle_b = b.sync.export_opts("default", true)?;
    let stats_a = a.sync.import_file(&bundle_b).await?;
    assert!(stats_a.slots_applied >= 1);
    assert_eq!(
        a.slots.get("roundtrip", Some("proj"))?.content,
        "v1 с alpha\n+ beta"
    );

    // Конфликт: A пишет свою (более новую) правку поверх beta-версии, затем
    // импортирует бандл B со «старой» строкой — входящая проигрывает LWW
    // и конфликт становится наблюдаемым (conflicts_lost)
    a.slots.replace("roundtrip", Some("proj"), "alpha переписала слот", "test", "test")?;
    a.db.with_conn(|c| {
        c.execute(
            "UPDATE slots SET updated_at = '2027-01-01T00:00:00Z' WHERE label = 'roundtrip'",
            [],
        )?;
        Ok(())
    })?;
    let bundle_b2 = b.sync.export_opts("default", true)?;
    let stats2 = a.sync.import_file(&bundle_b2).await?;
    assert!(stats2.conflicts_lost >= 1, "ожидали конфликт: {stats2:?}");
    assert_eq!(
        a.slots.get("roundtrip", Some("proj"))?.content,
        "alpha переписала слот",
        "LWW: локальная правка A новее бандла B и должна остаться"
    );

    // Reinforce на A (укрепление урока) → счётчик монотонно доезжает до B
    a.lessons.save(ob2h::lifecycle::LessonSaveRequest {
        content: "учить sync-урок тестово".into(),
        context: None,
        confidence: None,
        project: Some("proj".into()),
        tags: vec![],
        source: Some("manual".into()),
        source_ids: vec![],
        actor: "test".into(),
        tool: "test".into(),
    })?;
    let bundle_a2 = a.sync.export_opts("default", true)?;
    b.sync.import_file(&bundle_a2).await?;
    let reinforced = b
        .lessons
        .list(Some("proj"), None, 0.0, 50)?
        .into_iter()
        .find(|l| l.id == lesson.lesson.id)
        .expect("урок на B после reinforce");
    assert_eq!(reinforced.reinforcements, 1, "укрепление дошло до B");
    Ok(())
}
