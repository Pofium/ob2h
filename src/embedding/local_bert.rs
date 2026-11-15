//! Локальные in-process эмбеддинги на базе 100% чистого Rust фреймворка Candle (Hugging Face).
//! Загружает веса safetensors и вычисляет мультиязычные векторные представления на CPU без внешних зависимостей.

use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;
use tracing::info;

use super::EmbeddingProvider;
use crate::vector::normalize;

/// Размер батча по умолчанию для пакетного инференса Candle (Ф72.1).
pub const DEFAULT_CANDLE_BATCH_SIZE: usize = 32;

struct LocalBertInner {
    model: Mutex<BertModel>,
    tokenizer: Tokenizer,
    device: Device,
    dim: usize,
    max_seq_len: usize,
    batch_size: usize,
}

pub struct LocalBertEmbedding {
    inner: Arc<LocalBertInner>,
}

impl LocalBertEmbedding {
    pub fn new(model_id: &str) -> anyhow::Result<Self> {
        let repo_id = if model_id.is_empty() || model_id == "local" {
            "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
        } else {
            model_id
        };

        info!("Загрузка локальной модели эмбеддингов Candle: {repo_id}");
        let api = Api::new()?;
        let repo = api.repo(Repo::new(repo_id.to_string(), RepoType::Model));

        let config_filename = repo.get("config.json")?;
        let tokenizer_filename = repo.get("tokenizer.json")?;
        let weights_filename = repo.get("model.safetensors")?;

        let config: Config = serde_json::from_str(&std::fs::read_to_string(config_filename)?)?;
        let tokenizer = Tokenizer::from_file(tokenizer_filename)
            .map_err(|e| anyhow::anyhow!("Ошибка загрузки tokenizer.json: {e}"))?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_filename], DType::F32, &device)?
        };

        let model = BertModel::load(vb, &config)?;
        let dim = config.hidden_size;
        let max_seq_len = config.max_position_embeddings.max(1);
        let batch_size = std::env::var("OB2H_EMBED_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|v| v.clamp(1, 128))
            .unwrap_or(DEFAULT_CANDLE_BATCH_SIZE);

        info!("Локальная модель эмбеддингов успешно загружена (hidden_size: {dim}, batch_size: {batch_size})");

        Ok(Self {
            inner: Arc::new(LocalBertInner {
                model: Mutex::new(model),
                tokenizer,
                device,
                dim,
                max_seq_len,
                batch_size,
            }),
        })
    }
}

/// Подготовить батч тензоров `(token_ids, token_type_ids, attention_mask)` с паддингом до `max_len`.
fn build_batch_tensors(
    encodings: &[(&[u32], &[u32])],
    max_seq_len: usize,
    device: &Device,
) -> anyhow::Result<(Tensor, Tensor, Tensor)> {
    let batch_len = encodings.len();
    let max_len = encodings
        .iter()
        .map(|(ids, _)| ids.len().min(max_seq_len))
        .max()
        .unwrap_or(1)
        .max(1);

    let mut flat_ids = vec![0u32; batch_len * max_len];
    let mut flat_types = vec![0u32; batch_len * max_len];
    let mut flat_mask = vec![0u32; batch_len * max_len];

    for (row, (ids, types)) in encodings.iter().enumerate() {
        let len = ids.len().min(max_seq_len);
        let offset = row * max_len;
        flat_ids[offset..offset + len].copy_from_slice(&ids[..len]);
        let type_len = types.len().min(len);
        flat_types[offset..offset + type_len].copy_from_slice(&types[..type_len]);
        flat_mask[offset..offset + len].fill(1);
    }

    let ids_t = Tensor::from_vec(flat_ids, (batch_len, max_len), device)?;
    let types_t = Tensor::from_vec(flat_types, (batch_len, max_len), device)?;
    let mask_t = Tensor::from_vec(flat_mask, (batch_len, max_len), device)?;
    Ok((ids_t, types_t, mask_t))
}

/// Masked mean pooling по оси последовательности (`dim = 1`).
fn masked_mean_pool(output: &Tensor, attention_mask: &Tensor) -> anyhow::Result<Tensor> {
    let mask_f32 = attention_mask.to_dtype(DType::F32)?;
    let mask_3d = mask_f32.unsqueeze(2)?.broadcast_as(output.shape())?;
    let masked = (output * &mask_3d)?;
    let summed = masked.sum(1)?;
    let counts = mask_f32.sum(1)?.clamp(1e-9f32, f32::MAX)?.unsqueeze(1)?;
    Ok(summed.broadcast_div(&counts)?)
}

impl LocalBertInner {
    fn embed_sync(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = Vec::with_capacity(texts.len());
        let batch_size = self.batch_size.max(1);

        for chunk in texts.chunks(batch_size) {
            let mut encoded = Vec::with_capacity(chunk.len());
            for text in chunk {
                let enc = self
                    .tokenizer
                    .encode(text.as_str(), true)
                    .map_err(|e| anyhow::anyhow!("Tokenization error: {e}"))?;
                encoded.push(enc);
            }

            let slices: Vec<(&[u32], &[u32])> = encoded
                .iter()
                .map(|e| (e.get_ids(), e.get_type_ids()))
                .collect();

            let (ids_t, types_t, mask_t) =
                build_batch_tensors(&slices, self.max_seq_len, &self.device)?;

            let pooled = {
                let model = self
                    .model
                    .lock()
                    .map_err(|e| anyhow::anyhow!("Mutex lock error: {e}"))?;
                let output = model.forward(&ids_t, &types_t, Some(&mask_t))?;
                masked_mean_pool(&output, &mask_t)?
            };

            let rows: Vec<Vec<f32>> = pooled.to_vec2()?;
            for row in rows {
                results.push(normalize(&row));
            }
        }

        Ok(results)
    }
}

#[async_trait]
impl EmbeddingProvider for LocalBertEmbedding {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let inner = Arc::clone(&self.inner);
        let owned = texts.to_vec();
        tokio::task::spawn_blocking(move || inner.embed_sync(&owned))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking error for embedder: {e}"))?
    }

    fn dim(&self) -> usize {
        self.inner.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_batch_tensors_padding_and_truncation() {
        let device = Device::Cpu;
        let a_ids = [101u32, 10, 20, 102];
        let a_types = [0u32, 0, 0, 0];
        let b_ids = [101u32, 30, 102];
        let b_types = [0u32, 0, 0];
        let c_ids = [101u32, 40, 50, 60, 70, 102];
        let c_types = [0u32, 0, 0, 0, 0, 0];

        let (ids, _types, mask) = build_batch_tensors(
            &[
                (&a_ids, &a_types),
                (&b_ids, &b_types),
                (&c_ids, &c_types),
            ],
            5,
            &device,
        )
        .unwrap();

        assert_eq!(ids.dims(), &[3, 5]);
        assert_eq!(mask.dims(), &[3, 5]);
        let mask_rows = mask.to_vec2::<u32>().unwrap();
        assert_eq!(mask_rows[0], vec![1, 1, 1, 1, 0]);
        assert_eq!(mask_rows[1], vec![1, 1, 1, 0, 0]);
        assert_eq!(mask_rows[2], vec![1, 1, 1, 1, 1]);
    }

    #[test]
    fn test_masked_mean_pool_ignores_padding() {
        let device = Device::Cpu;
        // [batch=2, seq=3, hidden=2]
        let data = vec![
            1.0f32, 3.0, // token 0
            3.0, 5.0, // token 1
            999.0, 999.0, // padded token 2 (must be ignored!)
            2.0, 4.0, // batch 1 token 0
            999.0, 999.0, // padded
            999.0, 999.0, // padded
        ];
        let output = Tensor::from_vec(data, (2, 3, 2), &device).unwrap();
        let mask = Tensor::from_vec(vec![1u32, 1, 0, 1, 0, 0], (2, 3), &device).unwrap();
        let pooled = masked_mean_pool(&output, &mask).unwrap().to_vec2::<f32>().unwrap();

        assert!((pooled[0][0] - 2.0).abs() < 1e-5);
        assert!((pooled[0][1] - 4.0).abs() < 1e-5);
        assert!((pooled[1][0] - 2.0).abs() < 1e-5);
        assert!((pooled[1][1] - 4.0).abs() < 1e-5);
    }
}
