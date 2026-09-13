//! Ф21: bench на фиксированных векторах-заглушках — без сети и без fastembed (§7 AGENTS.md).

use std::sync::Arc;

use ob2h::cli::bench::{run_bench, GoldenCase};
use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::memory::MemoryService;

fn case(query: &str, keys: &[String]) -> GoldenCase {
    GoldenCase {
        query: query.to_string(),
        expect_keys: keys.to_vec(),
        note: String::new(),
    }
}

#[tokio::test]
async fn bench_search_and_context_on_fixed_vectors() {
    let db = Database::in_memory().expect("db in memory");
    let embedder = Arc::new(FakeEmbedding::new(384));
    let memory = MemoryService::new(db, embedder);

    let k1 = memory
        .save("Пользователь предпочитает кофе без сахара", Some("hmem-kofe"), "preferences", 0.9, "chat", None)
        .await
        .expect("save k1");
    let k2 = memory
        .save("Проект ob2h написан на Rust и SQLite", Some("hmem-rust"), "tech", 0.7, "chat", None)
        .await
        .expect("save k2");
    let _k3 = memory
        .save("Синхронизация PC и VPS идёт бандлами JSONL", Some("hmem-sync"), "infra", 0.6, "chat", None)
        .await
        .expect("save k3");

    let cases = vec![case("кофе", &[k1.clone()]), case("rust", &[k2.clone()])];

    // search: limit = max(k) = 3, записей всего 3 → цели гарантированно в выдаче
    let res = run_bench(&memory, &cases, "search", &[1, 3]).await.expect("bench search");
    assert_eq!(res.cases, 2);
    assert!((res.recall[1] - 1.0).abs() < 1e-9, "recall@3 должен быть 1.0");
    assert!(res.mrr > 0.0);
    assert!((0.0..=1.0).contains(&res.recall[0])); // порядок md5-векторов не детерминируем
    assert_eq!(res.empty_count, 0);
    assert!(res.missing_keys.is_empty());

    // context: build_context(20) включает все записи → обе цели в блоке
    let res_ctx = run_bench(&memory, &cases, "context", &[1]).await.expect("bench context");
    assert_eq!(res_ctx.cases, 2);
    assert!((res_ctx.recall[0] - 1.0).abs() < 1e-9, "все записи в блоке → recall@1 = 1.0");
}

#[tokio::test]
async fn bench_reports_missing_keys_and_counts_them_out() {
    let db = Database::in_memory().expect("db in memory");
    let embedder = Arc::new(FakeEmbedding::new(384));
    let memory = MemoryService::new(db, embedder);

    let k1 = memory
        .save("Кириллические пути проверяются в тестах", Some("hmem-cyr"), "tech", 0.8, "chat", None)
        .await
        .expect("save");

    let cases = vec![
        case("кириллица", &[k1]),
        case("нет такой записи", &["hmem-ghost".to_string()]),
    ];

    let res = run_bench(&memory, &cases, "search", &[5]).await.expect("bench");
    assert_eq!(res.missing_keys, vec!["hmem-ghost".to_string()]);
    assert_eq!(res.cases, 1, "кейс только с отсутствующими ключами исключается");
    // md5-векторы не гарантируют релевантность (косинус может быть отрицательным
    // и отфильтроваться) — проверяем только диапазон, точность метрик покрывают юнит-тесты.
    assert!((0.0..=1.0).contains(&res.recall[0]));
    assert!(res.p50_ms >= 0.0);
}
