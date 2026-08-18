use std::sync::Arc;
use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::memory::MemoryService;

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
    let ctx = service.build_context(5, Some("кофе")).expect("build_context");
    assert!(ctx.contains("<agent_memory>"));
    assert!(ctx.contains("кофе"));

    // Forget
    let forgotten = service.forget(&k2).expect("forget");
    assert!(forgotten);
    assert!(service.get(&k2).expect("get").is_none());
}
