//! Локальные in-process эмбеддинги на CPU через ONNX Runtime (fastembed, Ф72.2).
//! Поддерживаются мультиязычные/кодовые модели: `multilingual-e5-small` (дефолт),
//! `jina-embeddings-v2-base-code`, `nomic-embed-text-v1.5` (matryoshka 256/384/768).

use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use tracing::info;

use super::onnx_spec::{apply_matryoshka, resolve_onnx_model_spec, OnnxModelSpec};
use super::EmbeddingProvider;

fn to_fastembed_enum(spec: &OnnxModelSpec) -> EmbeddingModel {
    match spec.canonical_id {
        "jina-embeddings-v2-base-code" => EmbeddingModel::JinaEmbeddingsV2BaseCode,
        "nomic-embed-text-v1.5" => EmbeddingModel::NomicEmbedTextV15Q,
        _ => EmbeddingModel::MultilingualE5Small,
    }
}

struct FastembedInner {
    model: Mutex<TextEmbedding>,
    spec: OnnxModelSpec,
}

pub struct FastembedLocal {
    inner: Arc<FastembedInner>,
}

impl FastembedLocal {
    pub fn new(model_name: &str) -> anyhow::Result<Self> {
        let spec = resolve_onnx_model_spec(model_name);
        let embedding_model = to_fastembed_enum(&spec);
        info!(
            "Инициализация локальной ONNX модели fastembed: {:?} ({}, dim={})",
            embedding_model, spec.canonical_id, spec.dim
        );

        let options = InitOptions::new(embedding_model).with_show_download_progress(false);
        let model = TextEmbedding::try_new(options)?;

        Ok(Self {
            inner: Arc::new(FastembedInner {
                model: Mutex::new(model),
                spec,
            }),
        })
    }

    pub fn canonical_id(&self) -> &'static str {
        self.inner.spec.canonical_id
    }
}

#[async_trait]
impl EmbeddingProvider for FastembedLocal {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let inner = Arc::clone(&self.inner);
        let owned = texts.to_vec();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Vec<f32>>> {
            let str_refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
            let raw_embeddings = {
                let lock = inner
                    .model
                    .lock()
                    .map_err(|e| anyhow::anyhow!("Fastembed lock error: {e}"))?;
                lock.embed(str_refs, Some(32))?
            };

            let m_dim = inner.spec.matryoshka_dim;
            Ok(raw_embeddings
                .into_iter()
                .map(|v| apply_matryoshka(v, m_dim))
                .collect())
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking error for fastembed: {e}"))?
    }

    fn dim(&self) -> usize {
        self.inner.spec.dim
    }

    fn query_prefix(&self) -> &'static str {
        super::onnx_spec::prefixes_for_model(self.inner.spec.canonical_id).0
    }

    fn document_prefix(&self) -> &'static str {
        super::onnx_spec::prefixes_for_model(self.inner.spec.canonical_id).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_onnx_model_spec_multilingual_default() {
        let s = resolve_onnx_model_spec("");
        assert_eq!(s.canonical_id, "multilingual-e5-small");
        assert_eq!(s.dim, 384);

        // Англоязычные модели не допускаются как основная модель
        let rejected = resolve_onnx_model_spec("bge-small-en-v1.5");
        assert_eq!(rejected.canonical_id, "multilingual-e5-small");
    }

    #[test]
    fn test_resolve_onnx_model_spec_code_and_nomic_matryoshka() {
        let jina = resolve_onnx_model_spec("jina-embeddings-v2-base-code");
        assert_eq!(jina.canonical_id, "jina-embeddings-v2-base-code");
        assert_eq!(jina.dim, 768);

        let nomic = resolve_onnx_model_spec("nomic-embed-text-v1.5-384");
        assert_eq!(nomic.canonical_id, "nomic-embed-text-v1.5");
        assert_eq!(nomic.dim, 384);
        assert_eq!(nomic.matryoshka_dim, Some(384));
    }

    #[test]
    fn test_apply_matryoshka_truncates_and_normalizes() {
        let v = vec![3.0f32, 4.0, 100.0, 200.0];
        let out = apply_matryoshka(v, Some(2));
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.6).abs() < 1e-5);
        assert!((out[1] - 0.8).abs() < 1e-5);
    }
}
