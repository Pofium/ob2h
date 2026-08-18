//! Локальные in-process эмбеддинги на базе 100% чистого Rust фреймворка Candle (Hugging Face).
//! Загружает веса safetensors и вычисляет мультиязычные векторные представления на CPU без внешних зависимостей.

use std::sync::Mutex;
use async_trait::async_trait;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;
use tracing::info;

use super::EmbeddingProvider;
use crate::vector::normalize;

pub struct LocalBertEmbedding {
    model: Mutex<BertModel>,
    tokenizer: Tokenizer,
    device: Device,
    dim: usize,
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
            VarBuilder::from_mmaped_safetensors(&[weights_filename], candle_core::DType::F32, &device)?
        };

        let model = BertModel::load(vb, &config)?;
        let dim = config.hidden_size;

        info!("Локальная модель эмбеддингов успешно загружена (hidden_size: {dim})");

        Ok(Self {
            model: Mutex::new(model),
            tokenizer,
            device,
            dim,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for LocalBertEmbedding {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = Vec::with_capacity(texts.len());

        for text in texts {
            let encoding = self
                .tokenizer
                .encode(text.as_str(), true)
                .map_err(|e| anyhow::anyhow!("Tokenization error: {e}"))?;

            let tokens = encoding.get_ids();
            let token_type_ids = encoding.get_type_ids();

            let token_ids_tensor = Tensor::new(tokens, &self.device)?.unsqueeze(0)?;
            let token_type_ids_tensor = Tensor::new(token_type_ids, &self.device)?.unsqueeze(0)?;

            let embeddings = {
                let model = self.model.lock().map_err(|e| anyhow::anyhow!("Mutex lock error: {e}"))?;
                // Forward pass through BERT model
                let output = model.forward(&token_ids_tensor, &token_type_ids_tensor, None)?;
                
                // Mean pooling over token sequence dimension
                let (_n_sentence, n_tokens, _hidden_size) = output.dims3()?;
                let sum = output.sum(1)?;
                let mean = (sum / (n_tokens as f64))?;
                mean.squeeze(0)?
            };

            let vec: Vec<f32> = embeddings.to_vec1()?;
            results.push(normalize(&vec));
        }

        Ok(results)
    }

    fn dim(&self) -> usize {
        self.dim
    }
}
