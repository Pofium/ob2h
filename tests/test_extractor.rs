use std::sync::Arc;
use ob2h::extractor::{split_into_chunks, split_sentences, Extractor};
use ob2h::llm::{FakeLLM, LLMClient};

/// LLM-заглушка, которая всегда падает (как при 401 из-за неверного ключа).
struct FailingLLM;

#[async_trait::async_trait]
impl LLMClient for FailingLLM {
    async fn ask(&self, _prompt: &str, _system: Option<&str>) -> anyhow::Result<String> {
        Err(anyhow::anyhow!("LLM request failed after 3 attempts"))
    }
}

#[tokio::test]
async fn test_sentence_splitting_and_chunking() {
    let text = "Это первое предложение. Это второе! А вот и третье? Да, именно так.";
    let sents = split_sentences(text);
    assert_eq!(sents.len(), 4);

    let chunks = split_into_chunks(text, 50, 10);
    assert!(!chunks.is_empty());
}

#[tokio::test]
async fn test_extractor_with_fake_llm() {
    let fake_llm = Arc::new(FakeLLM::new());
    let json_resp = r#"{
        "entities": [
            {"id": "e1", "label": "Иван", "type": "Person", "description": "основатель компании"},
            {"id": "e2", "label": "OmnesCorp", "type": "Organization", "description": "ИТ-компания"}
        ],
        "relations": [
            {"source": "e1", "target": "e2", "label": "founded", "contexts": ["Иван основал OmnesCorp"]}
        ]
    }"#;
    fake_llm.set_default_response(json_resp);

    let extractor = Extractor::new(fake_llm, 10);
    let sample_text = "Иван основал компанию OmnesCorp в 2026 году в Москве. Это крупная международная организация, занимающаяся разработкой интеллектуальных систем и персональных агентов.";
    let result = extractor.extract(sample_text).await.expect("extract");

    assert_eq!(result.entities.len(), 2);
    assert_eq!(result.relations.len(), 1);
    assert_eq!(result.relations[0].label, "founded");
}

#[tokio::test]
async fn test_extractor_counts_llm_failures() {
    // Симптом «entities=0 при проиндексированных чанках»: ошибки LLM не теряются.
    let extractor = Extractor::new(Arc::new(FailingLLM), 10);
    let text = "Иван основал компанию OmnesCorp в 2026 году в Москве. Это крупная международная организация, занимающаяся разработкой интеллектуальных систем и персональных агентов.";
    let result = extractor.extract(text).await.expect("extract");

    assert!(result.entities.is_empty());
    assert!(result.chunks_processed > 0);
    assert_eq!(result.llm_failed, result.chunks_processed);
    let err = result.last_error.expect("last_error recorded");
    assert!(err.contains("failed after 3 attempts"));
}
