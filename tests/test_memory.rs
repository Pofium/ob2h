use std::sync::Arc;
use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::memory::{ContextOptions, MemoryService};

#[tokio::test]
async fn test_memory_crud_and_hybrid_search() {
    let db = Database::in_memory().expect("db in memory");
    let embedder = Arc::new(FakeEmbedding::new(384));
    let service = MemoryService::new(db, embedder);

    let k1 = service
        .save("Пользователь любит кофе без сахара", None, "preferences", 0.9, "chat", None)
        .await
        .expect("save k1");
    let k2 = service
        .save("Проект написан на Rust и SQLite", None, "tech", 0.7, "chat", None)
        .await
        .expect("save k2");

    let rec1 = service.get(&k1).expect("get").expect("record 1 exists");
    assert_eq!(rec1.category, "preferences");
    assert!((rec1.importance - 0.9).abs() < 1e-4);

    // Поиск
    let hits = service.search_hybrid("кофе", 5, 0.0).await.expect("search");
    assert!(!hits.is_empty());
    assert_eq!(hits[0].record.key, k1);

    // Decay и Purge
    let _ = service.decay_importance(0.1).expect("decay");
    let rec1_decayed = service.get(&k1).expect("get").unwrap();
    assert!(rec1_decayed.importance < 0.9);

    // Build context
    let ctx = service
        .build_context(5, Some("кофе"), &ContextOptions::default())
        .await
        .expect("build_context");
    assert!(ctx.contains("<agent_memory>"));
    assert!(ctx.contains("кофе"));

    // Forget
    let forgotten = service.forget(&k2).expect("forget");
    assert!(forgotten);
    assert!(service.get(&k2).expect("get").is_none());
}

// ── Issue #2.3: ETA прогрева эмбеддера (degraded-режим memory_search) ──

use async_trait::async_trait;
use chrono::Utc;
use ob2h::embedding::EmbeddingProvider;
use ob2h::memory::format_warming_suffix;

/// Стаб «холодный старт»: is_ready() = false, вектора — как у Fake.
struct ColdEmbedding {
    inner: FakeEmbedding,
}

#[async_trait]
impl EmbeddingProvider for ColdEmbedding {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.inner.embed(texts).await
    }
    fn dim(&self) -> usize {
        self.inner.dim()
    }
    fn is_ready(&self) -> bool {
        false
    }
}

#[test]
fn test_warmup_eta_pure_logic_with_history() {
    let now = 1_700_000_000_i64;
    // Греется 30 с, средний прошлый прогрев 60 с → осталось 30.
    let eta = ob2h::memory::service::warmup_eta_from(false, Some(now - 30), now, Some(60.0));
    assert_eq!(eta.elapsed_secs, 30);
    assert_eq!(eta.left_secs, Some(30));
    assert_eq!(format_warming_suffix(&eta), "warming_seconds_left=30");

    // elapsed ≥ среднего → остаток 0, не отрицательный.
    let eta = ob2h::memory::service::warmup_eta_from(false, Some(now - 90), now, Some(60.0));
    assert_eq!(eta.left_secs, Some(0));
    assert_eq!(format_warming_suffix(&eta), "warming_seconds_left=0");
}

#[test]
fn test_warmup_eta_pure_logic_first_warmup() {
    let now = 1_700_000_000_i64;
    // Без истории прогревов — честный elapsed с пометкой «первый прогрев».
    let eta = ob2h::memory::service::warmup_eta_from(false, Some(now - 5), now, None);
    assert_eq!(eta.elapsed_secs, 5);
    assert!(eta.left_secs.is_none());
    let suffix = format_warming_suffix(&eta);
    assert!(suffix.contains("warming_seconds_elapsed=5"));
    assert!(suffix.contains("первый прогрев"));

    // Без метки старта elapsed 0.
    let eta = ob2h::memory::service::warmup_eta_from(false, None, now, None);
    assert_eq!(eta.elapsed_secs, 0);
}

#[test]
fn test_warmup_eta_service_marks_start_and_averages() {
    let db = Database::in_memory().expect("db");
    let cold = Arc::new(ColdEmbedding {
        inner: FakeEmbedding::new(384),
    });
    let service = MemoryService::new(db.clone(), cold);

    // Первый degraded-запрос: метка старта фиксируется, истории ещё нет.
    let eta = service.warmup_eta();
    assert_eq!(eta.elapsed_secs, 0);
    assert!(eta.left_secs.is_none());
    let started: i64 = db
        .get_kv("metrics.embedder_warm_started_ts")
        .expect("kv")
        .expect("started mark")
        .parse()
        .expect("int");
    assert!(started > 0);

    // Ручная история: греется 120 с, средний прошлый прогрев 300 с → осталось 180.
    db.set_kv("metrics.embedder_warm_started_ts", &(started - 120).to_string())
        .expect("set");
    db.set_kv("metrics.embedder_warm_avg_secs", "300.0").expect("set");
    db.set_kv("metrics.embedder_warm_samples", "2").expect("set");
    let eta = service.warmup_eta();
    assert_eq!(eta.elapsed_secs, 120);
    assert_eq!(eta.left_secs, Some(180));

    // Готовый эмбеддер: длительность прогрева (200 с) усредняется
    // (300 + (200−300)/3 = 266.7), метка старта сбрасывается.
    let db2 = Database::in_memory().expect("db");
    db2.set_kv(
        "metrics.embedder_warm_started_ts",
        &(Utc::now().timestamp() - 200).to_string(),
    )
    .expect("set");
    db2.set_kv("metrics.embedder_warm_avg_secs", "300.0").expect("set");
    db2.set_kv("metrics.embedder_warm_samples", "2").expect("set");
    let ready = MemoryService::new(db2.clone(), Arc::new(FakeEmbedding::new(384)));
    let _ = ready.warmup_eta();
    assert_eq!(
        db2.get_kv("metrics.embedder_warm_started_ts").unwrap().as_deref(),
        Some("0")
    );
    let avg: f64 = db2
        .get_kv("metrics.embedder_warm_avg_secs")
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();
    assert!((avg - 266.7).abs() < 0.5, "avg={avg}");
    assert_eq!(
        db2.get_kv("metrics.embedder_warm_samples").unwrap().as_deref(),
        Some("3")
    );
}
