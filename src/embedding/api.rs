//! OpenAI-совместимый клиент API эмбеддингов.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use super::EmbeddingProvider;
use crate::vector::normalize;

#[derive(Debug, Clone)]
pub struct ApiEmbedding {
    base_url: String,
    api_key: String,
    model: String,
    client: Client,
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    input: &'a [String],
    model: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

impl ApiEmbedding {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        let url = if base_url.ends_with('/') {
            format!("{base_url}embeddings")
        } else {
            format!("{base_url}/embeddings")
        };
        Self {
            base_url: url,
            api_key: api_key.to_string(),
            model: model.to_string(),
            client: Client::new(),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for ApiEmbedding {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut req = self.client.post(&self.base_url);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let body = EmbeddingRequest {
            input: texts,
            model: &self.model,
        };

        let resp = req.json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("API embedding error {status}: {err_text}");
        }

        let mut parsed: EmbeddingResponse = resp.json().await?;
        parsed.data.sort_by_key(|d| d.index);

        let embeddings: Vec<Vec<f32>> = parsed
            .data
            .into_iter()
            .map(|d| normalize(&d.embedding))
            .collect();

        Ok(embeddings)
    }

    fn dim(&self) -> usize {
        // По умолчанию для большинства моделей (или переопределяется при первом ответе)
        384
    }
}
