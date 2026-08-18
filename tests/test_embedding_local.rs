use ob2h::embedding::{EmbeddingProvider, FakeEmbedding};

#[tokio::test]
async fn test_fake_embedding_deterministic() {
    let provider = FakeEmbedding::new(384);
    assert_eq!(provider.dim(), 384);

    let texts = vec![
        "Привет, это проверка локальных эмбеддингов в Rust".to_string(),
        "Память личного агента Hermes".to_string(),
    ];

    let embs = provider.embed(&texts).await.expect("embed texts");
    assert_eq!(embs.len(), 2);
    assert_eq!(embs[0].len(), 384);
    assert_eq!(embs[1].len(), 384);

    let norm: f32 = embs[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4);
}
