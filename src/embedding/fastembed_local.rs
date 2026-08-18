//! Локальные in-process эмбеддинги на CPU через ONNX Runtime (fastembed).

use std::sync::Mutex;
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use tracing::info;

use super::EmbeddingProvider;
use crate::vector::normalize;

pub struct FastembedLocal {
    model: Mutex<TextEmbedding>,
    dim: usize,
}

impl FastembedLocal {
    pub fn new(model_name: &str) -> anyhow::Result<Self> {
        let (embedding_model, dim) = Self::select_model(model_name);
        info!("Инициализация локальной ONNX модели fastembed: {:?}", embedding_model);

        let options = InitOptions::new(embedding_model).with_show_download_progress(true);
        let model = TextEmbedding::try_new(options)?;

        Ok(Self {
            model: Mutex::new(model),
            dim,
        })
    }

    fn select_model(name: &str) -> (EmbeddingModel, usize) {
        let lower = name.to_lowercase();
        if lower.contains("multilingual-e5") || lower.contains("e5-small") || lower.contains("e5") {
            (EmbeddingModel::MultilingualE5Small, 384)
        } else {
            // По умолчанию - Paraphrase Multilingual MiniLM L12 v2 (русский + мультиязычный, 384d)
            (EmbeddingModel::ParaphraseMLMiniLML12V2, 384)
        }
    }
}

#[async_trait]
impl EmbeddingProvider for FastembedLocal {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let str_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

        // fastembed синхронный, блокируем mutex
        let raw_embeddings = {
            let mut lock = self.model.lock().map_err(|e| anyhow::anyhow!("Fastembed lock error: {e}"))?;
            lock.embed(str_refs, None)?
        };

        let normalized: Vec<Vec<f32>> = raw_embeddings
            .into_iter()
            .map(|v| normalize(&v))
            .collect();

        Ok(normalized)
    }

    fn dim(&self) -> usize {
        self.dim
    }
}
