//! Провайдеры векторных представлений (Embeddings).

pub mod api;
pub mod fake;
pub mod local_bert;

use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

use crate::config::Settings;
pub use api::ApiEmbedding;
pub use fake::FakeEmbedding;
pub use local_bert::LocalBertEmbedding;

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Получить эмбеддинги для списка текстов (батч).
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Размерность вектора.
    fn dim(&self) -> usize;
}

pub fn provider_for(settings: &Settings) -> Arc<dyn EmbeddingProvider> {
    match settings.embed_provider.as_str() {
        "api" => {
            info!("Используется API провайдер эмбеддингов: {}", settings.embed_model);
            Arc::new(ApiEmbedding::new(
                &settings.embed_base_url,
                &settings.embed_api_key,
                &settings.embed_model,
            ))
        }
        "fake" => {
            info!("Используется Fake провайдер эмбеддингов (offline test mode)");
            Arc::new(FakeEmbedding::new(384))
        }
        _ => {
            // "local" — дефолтный in-process Candle (мультиязычная модель на CPU, 100% Rust)
            info!("Инициализация встроенной локальной модели эмбеддингов (Candle / Safetensors)...");
            match LocalBertEmbedding::new(&settings.embed_model) {
                Ok(local) => Arc::new(local),
                Err(e) => {
                    warn!("Не удалось загрузить локальную модель эмбеддингов: {e}; переключение на fallback");
                    if !settings.embed_base_url.is_empty() {
                        Arc::new(ApiEmbedding::new(
                            &settings.embed_base_url,
                            &settings.embed_api_key,
                            &settings.embed_model,
                        ))
                    } else {
                        Arc::new(FakeEmbedding::new(384))
                    }
                }
            }
        }
    }
}
