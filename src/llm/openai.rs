//! OpenAI-совместимый клиент для вызова чат-моделей.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::warn;
use super::LLMClient;

#[derive(Clone)]
pub struct OpenAIClient {
    base_url: String,
    api_key: String,
    model: String,
    #[allow(dead_code)]
    timeout: Duration,
    max_retries: u32,
    client: Client,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

impl OpenAIClient {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        timeout_secs: f64,
        max_retries: u32,
    ) -> Self {
        let url = if base_url.ends_with('/') {
            format!("{base_url}chat/completions")
        } else {
            format!("{base_url}/chat/completions")
        };

        Self {
            base_url: url,
            api_key: api_key.to_string(),
            model: model.to_string(),
            timeout: Duration::from_secs_f64(timeout_secs),
            max_retries,
            client: Client::builder().timeout(Duration::from_secs_f64(timeout_secs)).build().unwrap_or_default(),
        }
    }
}

#[async_trait]
impl LLMClient for OpenAIClient {
    async fn ask(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<String> {
        let mut messages = Vec::new();
        if let Some(sys) = system {
            messages.push(ChatMessage {
                role: "system",
                content: sys,
            });
        }
        messages.push(ChatMessage {
            role: "user",
            content: prompt,
        });

        let body = ChatRequest {
            model: &self.model,
            messages,
            temperature: 0.2,
        };

        let mut attempts = 0;
        let mut backoff = Duration::from_millis(500);

        loop {
            attempts += 1;
            let mut req = self.client.post(&self.base_url);
            if !self.api_key.is_empty() {
                req = req.bearer_auth(&self.api_key);
            }

            match req.json(&body).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let parsed: ChatResponse = resp.json().await?;
                        if let Some(choice) = parsed.choices.into_iter().next() {
                            return Ok(choice.message.content.unwrap_or_default());
                        }
                        return Ok(String::new());
                    }
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    warn!("LLM request error {status}: {err} (attempt {attempts}/{})", self.max_retries);
                }
                Err(e) => {
                    warn!("LLM transport error: {e} (attempt {attempts}/{})", self.max_retries);
                }
            }

            if attempts >= self.max_retries {
                anyhow::bail!("LLM request failed after {attempts} attempts");
            }

            tokio::time::sleep(backoff).await;
            backoff *= 2;
        }
    }
}
