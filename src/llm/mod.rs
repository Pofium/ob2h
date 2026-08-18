//! LLM-клиент для взаимодействия с моделями (OpenAI-совместимый API).

pub mod fake;
pub mod openai;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use std::sync::Arc;

use crate::config::Settings;
pub use fake::FakeLLM;
pub use openai::OpenAIClient;

#[async_trait]
pub trait LLMClient: Send + Sync {
    /// Обычный текстовый запрос к модели.
    async fn ask(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<String>;
}

#[async_trait]
pub trait LLMClientExt {
    /// Запрос с ожиданием и валидацией JSON-ответа.
    async fn ask_json<T: DeserializeOwned + Send>(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<T>;
}

#[async_trait]
impl<C: ?Sized + LLMClient> LLMClientExt for C {
    async fn ask_json<T: DeserializeOwned + Send>(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<T> {
        let text = self.ask(prompt, system).await?;
        let clean = clean_json_markdown(&text);
        let parsed = serde_json::from_str::<T>(&clean)
            .map_err(|e| anyhow::anyhow!("Failed to parse LLM JSON: {e}\nRaw output: {text}"))?;
        Ok(parsed)
    }
}

/// Очистка ответа модели от markdown ```json ... ``` обёрток.
pub fn clean_json_markdown(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(stripped) = trimmed.strip_prefix("```json") {
        if let Some(end) = stripped.strip_suffix("```") {
            return end.trim().to_string();
        }
    }
    if let Some(stripped) = trimmed.strip_prefix("```") {
        if let Some(end) = stripped.strip_suffix("```") {
            return end.trim().to_string();
        }
    }
    trimmed.to_string()
}

pub fn make_llm(settings: &Settings) -> Arc<dyn LLMClient> {
    if settings.llm_api_key.is_empty() && settings.llm_base_url.is_empty() {
        Arc::new(FakeLLM::default())
    } else {
        Arc::new(OpenAIClient::new(
            &settings.llm_base_url,
            &settings.llm_api_key,
            &settings.llm_model,
            settings.llm_timeout_secs,
            settings.llm_max_retries,
        ))
    }
}
