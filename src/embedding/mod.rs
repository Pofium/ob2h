//! Провайдеры векторных представлений (Embeddings).

pub mod api;
pub mod fake;
pub mod local_bert;

use async_trait::async_trait;
use std::sync::{Arc, OnceLock};
use tracing::{info, warn};

use crate::config::Settings;
pub use api::ApiEmbedding;
pub use fake::FakeEmbedding;
pub use local_bert::LocalBertEmbedding;

/// Ф25.1: реально активный бэкенд ("local_bert"|"api"|"fake") — для stats/doctor
/// и маркера деградации в выдаче поиска.
static ACTIVE_BACKEND: OnceLock<String> = OnceLock::new();

pub fn active_backend() -> &'static str {
    ACTIVE_BACKEND.get().map(|s| s.as_str()).unwrap_or("unknown")
}

fn set_backend(name: &str) {
    let _ = ACTIVE_BACKEND.set(name.to_string());
}

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
            set_backend("api");
            Arc::new(ApiEmbedding::new(
                &settings.embed_base_url,
                &settings.embed_api_key,
                &settings.embed_model,
            ))
        }
        "fake" => {
            info!("Используется Fake провайдер эмбеддингов (offline test mode)");
            set_backend("fake");
            Arc::new(FakeEmbedding::new(384))
        }
        _ => {
            // "local" — дефолтный in-process Candle (мультиязычная модель на CPU, 100% Rust)
            info!("Инициализация встроенной локальной модели эмбеддингов (Candle / Safetensors)...");
            match LocalBertEmbedding::new(&settings.embed_model) {
                Ok(local) => {
                    set_backend("local_bert");
                    Arc::new(local)
                }
                Err(e) => {
                    warn!("Не удалось загрузить локальную модель эмбеддингов: {e}; переключение на fallback");
                    if !settings.embed_base_url.is_empty() {
                        set_backend("api");
                        Arc::new(ApiEmbedding::new(
                            &settings.embed_base_url,
                            &settings.embed_api_key,
                            &settings.embed_model,
                        ))
                    } else {
                        // Ф25.1: деградация не молчит — backend зафиксирован, search/doctor/status
                        // помечают выдачу; векторный режим недостоверен.
                        warn!("API-фолбэк не настроен — эмбеддинги деградировали до Fake (hash-векторы)");
                        set_backend("fake");
                        Arc::new(FakeEmbedding::new(384))
                    }
                }
            }
        }
    }
}
