//! Провайдеры векторных представлений (Embeddings).

pub mod api;
pub mod fake;
#[cfg(feature = "onnx")]
pub mod fastembed_local;
pub mod fingerprint;
pub mod local_bert;
pub mod onnx_spec;

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

    /// Идентификатор бэкенда для `EmbeddingFingerprint` (Ф72.3).
    fn backend_id(&self) -> &'static str {
        active_backend()
    }

    /// Идентификатор модели для `EmbeddingFingerprint` (Ф72.3).
    fn model_id(&self) -> &str {
        "default"
    }

    /// Готов ли провайдер к немедленному инференсу (false во время холодного старта ONNX/Candle, Ф72.5).
    fn is_ready(&self) -> bool {
        true
    }

    /// Запустить фоновый прогрев весов без блокировки вызывающего потока (Ф72.5).
    fn trigger_warmup(&self) {}

    /// Префикс поискового запроса (Ф72.2b, напр. `"query: "` для E5).
    fn query_prefix(&self) -> &'static str {
        ""
    }

    /// Префикс индексируемого документа (Ф72.2b, напр. `"passage: "` для E5).
    fn document_prefix(&self) -> &'static str {
        ""
    }

    /// Векторизация поискового запроса с авто-подстановкой `query_prefix()`.
    async fn embed_query(&self, query: &str) -> anyhow::Result<Vec<f32>> {
        let p = self.query_prefix();
        let text = if p.is_empty() || query.starts_with(p) {
            query.to_string()
        } else {
            format!("{p}{query}")
        };
        let mut res = self.embed(&[text]).await?;
        res.pop()
            .ok_or_else(|| anyhow::anyhow!("empty embedding result for query"))
    }

    /// Векторизация документов с авто-подстановкой `document_prefix()`.
    async fn embed_documents(&self, docs: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let p = self.document_prefix();
        if p.is_empty() {
            return self.embed(docs).await;
        }
        let prefixed: Vec<String> = docs
            .iter()
            .map(|d| {
                if d.starts_with(p) {
                    d.clone()
                } else {
                    format!("{p}{d}")
                }
            })
            .collect();
        self.embed(&prefixed).await
    }
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

/// Ленивая обертка для неблокирующего старта сервиса (Фаза 61.3, 72.5).
/// Веса Candle загружаются в spawn_blocking только при первом реальном вызове embed() или warmup.
pub struct LazyEmbeddingProvider {
    inner: Arc<tokio::sync::OnceCell<Arc<dyn EmbeddingProvider>>>,
    warming: std::sync::atomic::AtomicBool,
    settings: Settings,
}

impl LazyEmbeddingProvider {
    pub fn new(settings: Settings) -> Self {
        Self {
            inner: Arc::new(tokio::sync::OnceCell::new()),
            warming: std::sync::atomic::AtomicBool::new(false),
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

    fn query_prefix(&self) -> &'static str {
        onnx_spec::prefixes_for_model(&self.settings.embed_model).0
    }

    fn document_prefix(&self) -> &'static str {
        onnx_spec::prefixes_for_model(&self.settings.embed_model).1
    }

    fn is_ready(&self) -> bool {
        self.inner.get().is_some()
    }

    fn trigger_warmup(&self) {
        if self.is_ready()
            || self
                .warming
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let cell = Arc::clone(&self.inner);
        let s = self.settings.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = cell
                    .get_or_try_init(|| async {
                        tokio::task::spawn_blocking(move || init_local_sync(&s))
                            .await
                            .map_err(|e| anyhow::anyhow!("warmup error: {e}"))
                    })
                    .await;
            });
        }
    }
}

#[cfg(feature = "onnx")]
struct LazyOnnxProvider {
    inner: tokio::sync::OnceCell<Arc<dyn EmbeddingProvider>>,
    settings: Settings,
    expected_dim: usize,
}

#[cfg(feature = "onnx")]
impl LazyOnnxProvider {
    fn new(settings: Settings) -> Self {
        let spec = onnx_spec::resolve_onnx_model_spec(&settings.embed_model);
        Self {
            inner: tokio::sync::OnceCell::new(),
            settings,
            expected_dim: spec.dim,
        }
    }

    async fn get_inner(&self) -> anyhow::Result<&Arc<dyn EmbeddingProvider>> {
        self.inner
            .get_or_try_init(|| async {
                let s = self.settings.clone();
                tokio::task::spawn_blocking(move || -> Arc<dyn EmbeddingProvider> {
                    match fastembed_local::FastembedLocal::new(&s.embed_model) {
                        Ok(fe) => {
                            set_backend("onnx");
                            Arc::new(fe)
                        }
                        Err(e) => {
                            warn!("Не удалось загрузить ONNX-модель fastembed: {e}; fallback на Candle");
                            init_local_sync(&s)
                        }
                    }
                })
                .await
                .map_err(|e| anyhow::anyhow!("spawn_blocking error for onnx init: {e}"))
            })
            .await
    }
}

#[cfg(feature = "onnx")]
#[async_trait]
impl EmbeddingProvider for LazyOnnxProvider {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let inner = self.get_inner().await?;
        inner.embed(texts).await
    }

    fn dim(&self) -> usize {
        if let Some(inner) = self.inner.get() {
            inner.dim()
        } else {
            self.expected_dim
        }
    }

    fn query_prefix(&self) -> &'static str {
        let spec = onnx_spec::resolve_onnx_model_spec(&self.settings.embed_model);
        onnx_spec::prefixes_for_model(spec.canonical_id).0
    }

    fn document_prefix(&self) -> &'static str {
        let spec = onnx_spec::resolve_onnx_model_spec(&self.settings.embed_model);
        onnx_spec::prefixes_for_model(spec.canonical_id).1
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

    fn query_prefix(&self) -> &'static str {
        self.inner.query_prefix()
    }

    fn document_prefix(&self) -> &'static str {
        self.inner.document_prefix()
    }

    fn backend_id(&self) -> &'static str {
        self.inner.backend_id()
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    fn trigger_warmup(&self) {
        self.inner.trigger_warmup();
    }
}

/// Санитайзер текста перед векторизацией (Ф72.6): удаляет API-ключи, токены и секреты.
pub fn sanitize_for_embedding(text: &str) -> String {
    if !crate::lifecycle::redact::enabled() || text.is_empty() {
        return text.to_string();
    }
    crate::lifecycle::redact::redact_with(&[], text)
}

/// Выделенный пул воркеров с семафором для изоляции тяжелого инференса от Tokio MCP (Ф72.5).
pub struct EmbeddingWorkerPool {
    inner: Arc<dyn EmbeddingProvider>,
    semaphore: tokio::sync::Semaphore,
}

impl EmbeddingWorkerPool {
    pub const DEFAULT_CONCURRENCY: usize = 2;

    pub fn new(inner: Arc<dyn EmbeddingProvider>, concurrency: usize) -> Self {
        Self {
            inner,
            semaphore: tokio::sync::Semaphore::new(concurrency.clamp(1, 16)),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for EmbeddingWorkerPool {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let span = tracing::info_span!("embed", count = texts.len());
        let _enter = span.enter();
        let started = std::time::Instant::now();
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("EmbeddingWorkerPool semaphore closed: {e}"))?;
        let sanitized: Vec<String> = texts.iter().map(|t| sanitize_for_embedding(t)).collect();
        let res = self.inner.embed(&sanitized).await;
        crate::graph::telemetry::stage_metrics::record_us(
            "embed",
            started.elapsed().as_micros() as u64,
        );
        res
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn backend_id(&self) -> &'static str {
        self.inner.backend_id()
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn query_prefix(&self) -> &'static str {
        self.inner.query_prefix()
    }

    fn document_prefix(&self) -> &'static str {
        self.inner.document_prefix()
    }

    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    fn trigger_warmup(&self) {
        self.inner.trigger_warmup();
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
        "onnx" | "fastembed" => {
            #[cfg(feature = "onnx")]
            {
                let s = settings.clone();
                // Ленивая инициализация ONNX-модели в spawn_blocking (холодный старт <100мс)
                Arc::new(LazyOnnxProvider::new(s))
            }
            #[cfg(not(feature = "onnx"))]
            {
                warn!("OB2H_EMBED_PROVIDER=onnx задан, но бинарник собран без --features onnx; fallback на Candle");
                Arc::new(LazyEmbeddingProvider::new(settings.clone()))
            }
        }
        _ => {
            // "local" — ленивая асинхронная загрузка через LazyEmbeddingProvider
            Arc::new(LazyEmbeddingProvider::new(settings.clone()))
        }
    };
    let concurrency = std::env::var("OB2H_EMBED_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(EmbeddingWorkerPool::DEFAULT_CONCURRENCY);
    let pooled: Arc<dyn EmbeddingProvider> =
        Arc::new(EmbeddingWorkerPool::new(inner, concurrency));
    // Ф62.2: кэш эмбеддингов запросов поверх пула воркеров.
    Arc::new(CachedEmbeddingProvider::new(pooled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    struct ColdStartStub {
        ready: AtomicBool,
        warmup_calls: AtomicUsize,
    }

    #[async_trait]
    impl EmbeddingProvider for ColdStartStub {
        async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect())
        }

        fn dim(&self) -> usize {
            4
        }

        fn is_ready(&self) -> bool {
            self.ready.load(Ordering::Acquire)
        }

        fn trigger_warmup(&self) {
            self.warmup_calls.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn test_worker_pool_preserves_readiness_and_bounds_concurrency() {
        let stub = Arc::new(ColdStartStub {
            ready: AtomicBool::new(false),
            warmup_calls: AtomicUsize::new(0),
        });
        let pool = EmbeddingWorkerPool::new(stub.clone(), 2);
        assert!(!pool.is_ready());
        pool.trigger_warmup();
        assert_eq!(stub.warmup_calls.load(Ordering::Acquire), 1);

        stub.ready.store(true, Ordering::Release);
        assert!(pool.is_ready());
        let res = pool.embed(&["hello".to_string()]).await.unwrap();
        assert_eq!(res.len(), 1);
    }

    #[tokio::test]
    async fn test_worker_pool_sanitizes_secrets_before_embedding() {
        struct CaptureProvider(Mutex<Vec<String>>);
        #[async_trait]
        impl EmbeddingProvider for CaptureProvider {
            async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
                self.0.lock().unwrap().extend_from_slice(texts);
                Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect())
            }
            fn dim(&self) -> usize {
                4
            }
        }

        let cap = Arc::new(CaptureProvider(Mutex::new(Vec::new())));
        let pool = EmbeddingWorkerPool::new(cap.clone(), 2);
        let input = vec![
            "API_KEY=supersecretvalue12345 sk-abcdef1234567890abcdef Bearer tok_9876543210".to_string(),
        ];
        pool.embed(&input).await.unwrap();
        let seen = cap.0.lock().unwrap();
        assert!(!seen[0].contains("supersecretvalue12345"));
        assert!(!seen[0].contains("sk-abcdef1234567890abcdef"));
        assert!(!seen[0].contains("tok_9876543210"));
        assert!(seen[0].contains("[REDACTED"));
    }
}
