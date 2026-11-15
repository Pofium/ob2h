//! Детерминированный генератор эмбеддингов на основе MD5 для тестов и оффлайн-режима.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use super::EmbeddingProvider;
use crate::vector::normalize;

#[derive(Debug, Clone)]
pub struct FakeEmbedding {
    dim: usize,
    query_prefix: &'static str,
    document_prefix: &'static str,
}

impl FakeEmbedding {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            query_prefix: "",
            document_prefix: "",
        }
    }

    /// Конструктор с асимметричными префиксами для тестирования 72.2b.
    pub fn with_prefixes(dim: usize, query_prefix: &'static str, document_prefix: &'static str) -> Self {
        Self {
            dim,
            query_prefix,
            document_prefix,
        }
    }
}

impl Default for FakeEmbedding {
    fn default() -> Self {
        Self::new(384)
    }
}

#[async_trait]
impl EmbeddingProvider for FakeEmbedding {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            let hash = hasher.finalize();

            let mut vec = Vec::with_capacity(self.dim);
            for i in 0..self.dim {
                let byte = hash[i % hash.len()] as f32;
                let factor = ((i + 1) as f32).sin();
                vec.push((byte / 255.0 - 0.5) * factor);
            }
            results.push(normalize(&vec));
        }
        Ok(results)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn query_prefix(&self) -> &'static str {
        self.query_prefix
    }

    fn document_prefix(&self) -> &'static str {
        self.document_prefix
    }

    fn backend_id(&self) -> &'static str {
        "fake"
    }

    fn model_id(&self) -> &str {
        "fake-sha256"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fake_embedding_deterministic() {
        let embedder = FakeEmbedding::new(384);
        let texts = vec!["Привет мир".to_string(), "Тестовый текст".to_string()];
        let e1 = embedder.embed(&texts).await.unwrap();
        let e2 = embedder.embed(&texts).await.unwrap();

        assert_eq!(e1.len(), 2);
        assert_eq!(e1[0].len(), 384);
        assert_eq!(e1[0], e2[0]);
        assert_ne!(e1[0], e1[1]);
    }

    #[tokio::test]
    async fn test_asymmetric_prefixes_auto_applied() {
        let plain = FakeEmbedding::new(64);
        let e5 = FakeEmbedding::with_prefixes(64, "query: ", "passage: ");

        let q_vec = e5.embed_query("тест").await.unwrap();
        let expected_q = plain.embed(&["query: тест".to_string()]).await.unwrap().pop().unwrap();
        assert_eq!(q_vec, expected_q);

        // Повторная подстановка не дублирует префикс
        let q_vec_dup = e5.embed_query("query: тест").await.unwrap();
        assert_eq!(q_vec_dup, expected_q);

        let d_vecs = e5.embed_documents(&["документ".to_string()]).await.unwrap();
        let expected_d = plain.embed(&["passage: документ".to_string()]).await.unwrap();
        assert_eq!(d_vecs, expected_d);
    }
}
