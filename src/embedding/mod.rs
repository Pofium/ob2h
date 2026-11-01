//! Провайдеры векторных представлений (Embeddings).

pub mod api;
pub mod fake;
pub mod local_bert;

use async_trait::async_trait;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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

/// Ф62.2: нормализация текста в ключ кэша (trim + lower-case).
fn cache_key(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Ф62.2: потокобезопасный LRU-кэш эмбеддингов запросов.
/// Повторные запросы агентов получают вектор из памяти (~0 мс) без вызова модели.
pub struct CachedEmbeddingProvider {
    inner: Arc<dyn EmbeddingProvider>,
    cache: Mutex<LruCache<String, Vec<f32>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CachedEmbeddingProvider {
    /// Ёмкость кэша по умолчанию — 1000 записей (Ф62.2).
    pub const DEFAULT_CAPACITY: usize = 1000;

    pub fn new(inner: Arc<dyn EmbeddingProvider>) -> Self {
        Self::with_capacity(inner, Self::DEFAULT_CAPACITY)
    }

    pub fn with_capacity(inner: Arc<dyn EmbeddingProvider>, capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("capacity > 0");
        Self {
            inner,
            cache: Mutex::new(LruCache::new(cap)),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Текущее число записей в кэше (для тестов/статистики).
    pub fn cache_len(&self) -> usize {
        self.cache.lock().unwrap().len()
    }

    /// (попадания, промахи) — для диагностики.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

#[async_trait]
impl EmbeddingProvider for CachedEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let keys: Vec<String> = texts.iter().map(|t| cache_key(t)).collect();
        // Слоты результата: Some(вектор) — из кэша, None — нужно посчитать.
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        let mut miss_idx: Vec<usize> = Vec::new();
        {
            let mut cache = self.cache.lock().unwrap();
            for (i, key) in keys.iter().enumerate() {
                match cache.get(key) {
                    Some(vec) => {
                        slots[i] = Some(vec.clone());
                        self.hits.fetch_add(1, Ordering::Relaxed);
                    }
                    None => {
                        miss_idx.push(i);
                        self.misses.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        if !miss_idx.is_empty() {
            let miss_texts: Vec<String> = miss_idx.iter().map(|&i| texts[i].clone()).collect();
            let fresh = self.inner.embed(&miss_texts).await?;
            if fresh.len() != miss_idx.len() {
                anyhow::bail!(
                    "embedding provider вернул {} векторов на {} текстов",
                    fresh.len(),
                    miss_idx.len()
                );
            }
            let mut cache = self.cache.lock().unwrap();
            for (slot_i, vec) in miss_idx.iter().zip(fresh) {
                cache.put(keys[*slot_i].clone(), vec.clone());
                slots[*slot_i] = Some(vec);
            }
        }

        slots
            .into_iter()
            .map(|s| s.ok_or_else(|| anyhow::anyhow!("embedding slot not filled")))
            .collect()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }
}

pub fn provider_for(settings: &Settings) -> Arc<dyn EmbeddingProvider> {
    let inner: Arc<dyn EmbeddingProvider> = match settings.embed_provider.as_str() {
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
    };
    // Ф62.2: кэш эмбеддингов запросов поверх любого бэкенда.
    Arc::new(CachedEmbeddingProvider::new(inner))
}
