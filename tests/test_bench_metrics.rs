//! Тесты bench-метрик полноты (v1.9.9, Ф69.2): recall-all@k / any-hit@k —
//! unit-скоринг + интеграция на синтетическом golden (все кейсы — полные хиты).

use std::collections::HashSet;

use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::cli::bench::{any_hit_at_k, recall_all_at_k, recall_at_k, run_bench};
use tempfile::tempdir;

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_recall_all_and_any_hit_scoring() {
    let ranked = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let expected = set(&["a", "c"]);

    // k=1: только a — частичный хит
    assert!(!recall_all_at_k(&ranked, &expected, 1));
    assert!(any_hit_at_k(&ranked, &expected, 1));
    // k=3: оба найдены
    assert!(recall_all_at_k(&ranked, &expected, 3));
    assert!(any_hit_at_k(&ranked, &expected, 3));
    // k=2: a и b — c не в топе
    assert!(!recall_all_at_k(&ranked, &expected, 2));
    assert!(any_hit_at_k(&ranked, &expected, 2));

    // Пустой expected — false (нет сигнала)
    assert!(!recall_all_at_k(&ranked, &set(&[]), 5));
    assert!(!any_hit_at_k(&ranked, &set(&[]), 5));
}

/// Обратная совместимость: старый golden (один ключ на кейс) — recall-all@k
/// математически совпадает с «найден/не найден» из recall_at_k.
#[test]
fn test_single_key_golden_equivalence() {
    let ranked = vec!["a".to_string(), "x".to_string(), "b".to_string()];
    for expected in [set(&["a"]), set(&["b"]), set(&["z"])] {
        for k in [1, 2, 3, 5] {
            let hit = recall_at_k(&ranked, &expected, k) > 0.0;
            assert_eq!(
                hit,
                recall_all_at_k(&ranked, &expected, k),
                "k={k}, expected={expected:?}"
            );
            assert_eq!(
                hit,
                any_hit_at_k(&ranked, &expected, k),
                "k={k}, expected={expected:?}"
            );
        }
    }
}

#[tokio::test]
async fn test_run_bench_reports_new_metrics() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();
    let ctx = init_app(settings)?;

    // Три записи; оба ключа кейса обязаны найтись (корпус весь в топ-5)
    ctx.memory
        .save("JWT авторизация: refresh-токены обновлены", None, "general", 0.5, "chat", None)
        .await?;
    ctx.memory
        .save("rate limiting: окно 60 секунд", None, "general", 0.5, "chat", None)
        .await?;

    let golden = tmp.path().join("golden.jsonl");
    std::fs::write(
        &golden,
        format!(
            "{}\n{}\n",
            serde_json::json!({"query": "JWT авторизация refresh", "expect_keys": ["кейс старого формата"], "note": "n/a"}),
            serde_json::json!({
                "query": "JWT rate limiting",
                "expect_keys": ["jwt-tok", "rate-lim"],
                "note": "новый формат: множество релевантных"
            })
        ),
    )?;

    // Ключи из golden не существуют — гигиена исключит их… но тогда expected
    // пуст и кейсы не скорятся. Для честного прогона приводим ключи к реальным:
    // читаем фактические ключи записей.
    let keys: Vec<String> = ctx.db.with_conn(|c| {
        let mut stmt = c.prepare("SELECT key FROM memories ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    })?;
    let new_golden = format!(
        "{}\n{}\n",
        serde_json::json!({"query": "JWT авторизация refresh", "expect_keys": [keys[0]], "note": "старый формат"}),
        serde_json::json!({"query": "rate limiting", "expect_keys": [keys[1], keys[0]], "note": "новый формат: множество релевантных"})
    );
    std::fs::write(&golden, new_golden)?;

    let cases = ob2h::cli::bench::load_golden(&golden)?;
    let result = run_bench(&ctx.memory, &cases, "search", &[5]).await?;
    assert_eq!(result.cases, 2);
    // Кейсы подобраны под детерминированную выдачу fake-гибрида (см. журнал §6):
    // «JWT авторизация refresh» → [k1]; «rate limiting» → [k2, k1] — оба ключа в топ-5.
    assert!((result.recall_all[0] - 1.0).abs() < 1e-9, "{:?}", result.recall_all);
    assert!((result.any_hit[0] - 1.0).abs() < 1e-9, "{:?}", result.any_hit);
    assert!((result.recall[0] - 1.0).abs() < 1e-9);
    Ok(())
}
