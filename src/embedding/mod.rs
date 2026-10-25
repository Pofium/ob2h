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

/// Синхронная инициализация локального бэкенда (вызывается внутри spawn_blocking)
fn init_local_sync(settings: &Settings) -> Arc<dyn EmbeddingProvider> {
    info!("Инициализация встроенной локальной модели эмбеддингов (Candle / Safetensors)...");
    match LocalBertEmbedding::new(&settings.embed_model) {
        Ok(local) => {
            set_backend("local_bert");
            Arc::new(local)
        }
        Err(e) => {
            warn!("Не удалось загрузить локальную модель эмбеддингов: {e}; переключение на fallback");
            if !settings.embed_base_url.is_empty() && !settings.offline {
                set_backend("api");
                Arc::new(ApiEmbedding::new(
                    &settings.embed_base_url,
                    &settings.embed_api_key,
                    &settings.embed_model,
                ))
            } else {
                warn!("API-фолбэк не настроен или включен offline-режим — эмбеддинги деградировали до Fake (hash-векторы)");
                set_backend("fake");
                Arc::new(FakeEmbedding::new(384))
            }
        }
    }
}

/// Ленивая обертка для неблокирующего старта сервиса (Фаза 61.3).
/// Веса Candle загружаются в spawn_blocking только при первом реальном вызове embed().
pub struct LazyEmbeddingProvider {
    inner: tokio::sync::OnceCell<Arc<dyn EmbeddingProvider>>,
    settings: Settings,
}

impl LazyEmbeddingProvider {
    pub fn new(settings: Settings) -> Self {
        Self {
            inner: tokio::sync::OnceCell::new(),
            settings,
        }
    }

    async fn get_inner(&self) -> anyhow::Result<&Arc<dyn EmbeddingProvider>> {
        self.inner
            .get_or_try_init(|| async {
                let s = self.settings.clone();
                tokio::task::spawn_blocking(move || init_local_sync(&s))
                    .await
                    .map_err(|e| anyhow::anyhow!("spawn_blocking error for embedder init: {e}"))
            })
            .await
    }
}

#[async_trait]
impl EmbeddingProvider for LazyEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let inner = self.get_inner().await?;
        inner.embed(texts).await
    }

    fn dim(&self) -> usize {
        if let Some(inner) = self.inner.get() {
            inner.dim()
        } else {
            384
        }
    }
}

pub fn provider_for(settings: &Settings) -> Arc<dyn EmbeddingProvider> {
    match settings.embed_provider.as_str() {
        "api" => {
            if settings.offline {
                warn!("OB2H_OFFLINE включен: API-провайдер заменён на FakeEmbedding");
                set_backend("fake");
                Arc::new(FakeEmbedding::new(384))
            } else {
                info!("Используется API провайдер эмбеддингов: {}", settings.embed_model);
                set_backend("api");
                Arc::new(ApiEmbedding::new(
                    &settings.embed_base_url,
                    &settings.embed_api_key,
                    &settings.embed_model,
                ))
            }
        }
        "fake" => {
            info!("Используется Fake провайдер эмбеддингов (offline test mode)");
            set_backend("fake");
            Arc::new(FakeEmbedding::new(384))
        }
        _ => {
            // "local" — ленивая асинхронная загрузка через LazyEmbeddingProvider
            Arc::new(LazyEmbeddingProvider::new(settings.clone()))
        }
    }
}
