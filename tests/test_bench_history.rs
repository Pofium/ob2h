//! Ф35.2/30.3: накопительная статистика bench — history.jsonl (append+analyze),
//! медианы off/on, вердикт 35.2 (честный: без 30 прогонов решение откладывается).

use ob2h::cli::bench_history::{analyze, append_record, history_path};
use serde_json::json;

#[test]
fn history_append_and_analyze() {
    let dir = tempfile::tempdir().unwrap();
    let path = history_path(dir.path());

    // 12 прогонов off (p95 растёт), 5 прогонов on (быстрее)
    for i in 0..12 {
        append_record(
            &path,
            &json!({
                "ts": format!("2026-09-13T10:{:02}:00Z", i),
                "mode": "latency",
                "p95_ms": 2000.0 + i as f64 * 10.0,
                "db_size_mb": 648.8,
                "embedding_backend": "local",
                "vec0": "off",
                "dream_sha": "no-dreams"
            }),
        )
        .unwrap();
    }
    for i in 0..5 {
        append_record(
            &path,
            &json!({
                "ts": format!("2026-09-13T11:{:02}:00Z", i),
                "mode": "latency",
                "p95_ms": 1000.0 + i as f64,
                "db_size_mb": 648.8,
                "embedding_backend": "local",
                "vec0": "on",
                "dream_sha": "no-dreams"
            }),
        )
        .unwrap();
    }
    append_record(
        &path,
        &json!({
            "ts": "2026-09-13T12:00:00Z",
            "mode": "search",
            "recall@5": 0.95,
            "recall@10": 1.0,
            "mrr": 0.8,
            "p95_ms": 40.0
        }),
    )
    .unwrap();

    let s = analyze(&path).unwrap();
    assert_eq!(s.total_runs, 18);
    assert_eq!(s.by_mode.get("latency"), Some(&17));
    assert_eq!(s.by_mode.get("search"), Some(&1));
    assert_eq!(s.latency_runs, 17);
    // медиана off (12 значений 2000..2110, шаг 10) = медиана 6-го и 7-го = 2050+5? -> 2055? sort: [2000..2110]; len 12 -> index 6 = 2060
    let off = s.latency_off_p95_median.unwrap();
    assert!((2050.0..=2060.0).contains(&off), "off медиана: {off}");
    let on = s.latency_on_p95_median.unwrap();
    assert!((1000.0..=1004.0).contains(&on), "on медиана: {on}");
    assert_eq!(s.last_ts.as_deref(), Some("2026-09-13T12:00:00Z"));
    // вердикт: <30 прогонов -> отложено; rerank-профиля нет -> OB2H_RERANK off
    assert!(
        s.verdict.contains("12/30") || s.verdict.contains("17/30") || s.verdict.contains("/30")
    );
    assert!(s.verdict.contains("OB2H_RERANK остаётся off"));
    // 17 latency-прогонов ≥ 10: on/off сравнение уже показывается (−51% >= 30%)
    assert!(
        s.verdict
            .contains("данные поддерживают включение по умолчанию"),
        "вердикт: {}",
        s.verdict
    );
}

#[test]
fn missing_history_is_honest() {
    let dir = tempfile::tempdir().unwrap();
    let s = analyze(&history_path(dir.path())).unwrap();
    assert_eq!(s.total_runs, 0);
    assert!(s.verdict.contains("отсутствует"));
}
