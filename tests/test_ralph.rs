//! Ф26–27: Ralph-ядро — авто-вердикты (ADR-K4), идемпотентность, AST-дельта,
//! staleness-pass, контекст-пакет с reuse-кандидатами, debt-леджер. Без сети.

use std::sync::Arc;

use tempfile::tempdir;

use ob2h::db::Database;
use ob2h::project::ProjectService;
use ob2h::ralph::RalphService;

fn setup() -> (tempfile::TempDir, RalphService, String) {
    let tmp = tempdir().expect("tempdir");
    let db = Database::in_memory().expect("db");
    let project = Arc::new(ProjectService::new(db.conn_arc()));
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn foo() -> i32 { 1 }\npub struct Settings;\n",
    )
    .expect("write fixture");
    let p = project
        .register_project(
            "ralphtest",
            "Ralph Test",
            &root.to_string_lossy(),
            None,
            None,
        )
        .expect("register project");
    let svc = RalphService::new(db, project, tmp.path().to_path_buf());
    (tmp, svc, p.id)
}

#[test]
fn m6_is_additive_and_preserves_manual_confidence_column() {
    // Спека §4.1: graph_nodes не пересоздаётся — ручная колонка confidence жива после M6.
    let db = Database::in_memory().expect("db");
    let has_confidence: bool = db
        .with_conn(|conn| {
            let n: i64 = conn.query_row(
                "SELECT count(*) FROM pragma_table_info('graph_nodes') WHERE name = 'confidence'",
                [],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
        .expect("schema check");
    assert!(has_confidence, "колонка confidence должна существовать после миграций");
}

#[test]
fn start_duplicate_feature_fails() {
    let (_tmp, svc, pid) = setup();
    let run = svc.start(&pid, "feat-a", "цель", None, None, None, None).expect("start");
    assert!(!run.is_empty());
    assert!(svc.start(&pid, "feat-a", "цель 2", None, None, None, None).is_err());
}

#[test]
fn iteration_auto_verdicts_follow_tests_not_self_assessment() {
    let (_tmp, svc, pid) = setup();
    let run = svc.start(&pid, "feat-v", "цель", None, None, None, None).expect("start");

    // Красная итерация: self_assessment говорит «ок», тесты красные → failed (ADR-K4)
    let red = svc
        .iteration(
            &run,
            "T-001",
            1,
            Some("гипотеза"),
            Some(r#"["шаг"]"#),
            Some(r#"{"what_done":"x","self_assessment":"ок"}"#),
            Some(r#"{"passed":0,"failed":2}"#),
            Some("minimal"),
            None,
            None,
            None,
        )
        .expect("iteration 1");
    assert_eq!(red.verdict, "failed");
    assert_eq!(red.verdict_source, "auto_tests");
    assert!(red.ast_changes >= 1, "первый скан фиксирует добавленные символы");

    // Зелёная итерация
    let green = svc
        .iteration(
            &run,
            "T-001",
            2,
            Some("исправили"),
            None,
            None,
            Some(r#"{"passed":3,"failed":0}"#),
            Some("reuse:foo"),
            None,
            None,
            None,
        )
        .expect("iteration 2");
    assert_eq!(green.verdict, "verified");

    // Идемпотентность: (run, task, n) уже записан
    assert!(svc
        .iteration(&run, "T-001", 2, None, None, None, Some(r#"{"passed":1}"#), None, None, None, None)
        .is_err());
}

#[test]
fn staleness_pass_marks_findings_on_symbol_change() {
    let (_tmp, svc, pid) = setup();
    let run = svc.start(&pid, "feat-s", "цель", None, None, None, None).expect("start");

    // verified-файндинг, привязанный к символу foo
    svc.add_finding(&pid, Some(&run), None, "gotcha", "foo считает неправильно", Some(r#"["fn:foo"]"#), None)
        .expect("seed finding");

    // Итерация 1: первичный скан
    svc.iteration(&run, "T-001", 1, None, None, None, Some(r#"{"passed":1}"#), None, None, None, None)
        .expect("iteration 1");

    // Переименование foo → bar: символ foo удалён → finding должен стать stale
    let root_string = svc.project_root(&pid).expect("project").expect("root");
    let root = std::path::Path::new(&root_string);
    std::fs::write(root.join("src/lib.rs"), "pub fn bar() -> i32 { 1 }\npub struct Settings;\n")
        .expect("rewrite fixture");

    let outcome = svc
        .iteration(&run, "T-001", 2, None, None, None, Some(r#"{"passed":1}"#), None, None, None, None)
        .expect("iteration 2");
    assert!(outcome.stale_marked >= 1, "finding по удалённому символу помечен stale");

    // Повторная итерация без изменений символа — не перемечает (FR-K5)
    let outcome3 = svc
        .iteration(&run, "T-001", 3, None, None, None, Some(r#"{"passed":1}"#), None, None, None, None)
        .expect("iteration 3");
    assert_eq!(outcome3.stale_marked, 0, "уже stale — повторно не перемечается");
}

#[test]
fn context_pack_contains_spec_fragment_and_reuse_candidates() {
    let (tmp, svc, pid) = setup();
    let run = svc.start(&pid, "feat-c", "цель", None, None, None, None).expect("start");

    // Спека фичи в репозитории проекта
    let root = tmp.path().join("repo");
    let spec_dir = root.join("openspec/changes/feat-c");
    std::fs::create_dir_all(&spec_dir).expect("spec dir");
    std::fs::write(spec_dir.join("proposal.md"), "WHEN конфиг меняется THEN кэш инвалидируется за 1с")
        .expect("write spec");

    // Первичный скан: символы foo попадают в reuse-кандидаты
    svc.iteration(&run, "T-001", 1, None, None, None, Some(r#"{"passed":1}"#), None, None, None, None)
        .expect("iteration");

    let ctx = svc.context(&run, "T-001", 6000, "full").expect("context");
    assert!(ctx.contains("<ralph_context"), "{ctx}");
    assert!(ctx.contains("кэш инвалидируется"), "фрагмент спеки в пакете: {ctx}");
    assert!(ctx.contains("reuse_candidates"), "reuse-блок присутствует: {ctx}");
    assert!(ctx.contains("foo"), "существующий символ — reuse-кандидат: {ctx}");
    assert!(ctx.contains("context_ref"), "аудит-ссылка на файл пакета: {ctx}");

    let lite = svc.context(&run, "T-001", 6000, "lite").expect("context lite");
    assert!(!lite.contains("architecture_zone"), "lite не содержит архитектурную зону");
}

#[test]
fn report_shows_debt_ledger_and_reuse_rate() {
    let (_tmp, svc, pid) = setup();
    let run = svc.start(&pid, "feat-d", "цель", None, None, None, None).expect("start");

    svc.add_finding(
        &pid,
        Some(&run),
        None,
        "deferred",
        "упрощённая валидация",
        None,
        Some(r#"{"ceiling":"нет max длины","no_trigger":1}"#),
    )
    .expect("seed deferred");

    svc.iteration(
        &run,
        "T-001",
        1,
        None,
        None,
        None,
        Some(r#"{"passed":2}"#),
        Some("reuse:foo"),
        None,
        None,
        None,
    )
    .expect("iteration");

    let report = svc.report(Some(&run)).expect("report");
    assert!(report.contains("reuse-hit=100%"), "{report}");
    assert!(report.contains("debt-леджер"), "{report}");
    assert!(report.contains("no-trigger!"), "deferred без триггера подсвечен: {report}");

    let history = svc.ast_history(&pid, "foo").expect("history");
    assert!(history.contains("added"), "хронология символа: {history}");

    let diff = svc.ast_diff(&pid, &run, None).expect("diff");
    assert!(!diff.is_empty());
}
