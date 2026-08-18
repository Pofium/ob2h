//! Детерминированный генератор эмбеддингов на основе MD5 для тестов и оффлайн-режима.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use super::EmbeddingProvider;
use crate::vector::normalize;

#[derive(Debug, Clone)]
pub struct FakeEmbedding {
    dim: usize,
}

impl FakeEmbedding {
    pub fn new(dim: usize) -> Self {
        Self { dim }
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
}
