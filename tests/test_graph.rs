use std::sync::Arc;
use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::extractor::{Entity, ExtractionResult, Relation};
use ob2h::graph::GraphService;
use ob2h::llm::FakeLLM;

#[tokio::test]
async fn test_graph_upsert_search_and_reason() {
    let db = Database::in_memory().expect("db in memory");
    let embedder = Arc::new(FakeEmbedding::new(384));
    let service = GraphService::new(db, embedder);

    let extracted = ExtractionResult {
        entities: vec![
            Entity {
                label: "Анна".to_string(),
                entity_type: "Person".to_string(),
                description: "главный архитектор системы".to_string(),
            },
            Entity {
                label: "OmnesBot".to_string(),
                entity_type: "Artifact".to_string(),
                description: "ИИ-агент для работы".to_string(),
            },
        ],
        relations: vec![Relation {
            source: "Анна".to_string(),
            target: "OmnesBot".to_string(),
            label: "created".to_string(),
            contexts: vec!["Анна создала OmnesBot".to_string()],
        }],
        chunks_processed: 1,
        chunks_skipped: 0,
    };

    let (new_e, up_e, new_r) = service.upsert_extraction(&extracted).await.expect("upsert");
    assert_eq!(new_e, 2);
    assert_eq!(up_e, 0);
    assert_eq!(new_r, 1);

    // Повторный upsert увеличивает val и не дублирует
    let (new_e2, up_e2, _) = service.upsert_extraction(&extracted).await.expect("upsert 2");
    assert_eq!(new_e2, 0);
    assert_eq!(up_e2, 2);

    // Поиск по графу
    let found = service.search("Анна", 5, true).await.expect("search");
    assert_eq!(found.nodes.len(), 2); // 1-hop соседи: Анна + OmnesBot
    assert_eq!(found.edges.len(), 1);

    // KAG Reason
    let fake_llm = Arc::new(FakeLLM::new());
    fake_llm.set_default_response(r#"{
        "answer": "Анна является создателем агента OmnesBot.",
        "confidence": 0.95,
        "reasoning_steps": ["Найдена сущность Анна", "Найдена связь created"],
        "used_entities": ["Анна", "OmnesBot"],
        "used_relations": ["created"]
    }"#);

    let reason_res = service.reason("Кто создал OmnesBot?", fake_llm).await.expect("reason");
    assert!(reason_res.confidence > 0.9);
    assert!(reason_res.answer.contains("создателем"));
}
