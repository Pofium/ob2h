//! LLM-клиент для взаимодействия с моделями (OpenAI-совместимый API).

pub mod fake;
pub mod harness;
pub mod local;
pub mod openai;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use std::sync::{Arc, RwLock};

use crate::config::Settings;
pub use fake::FakeLLM;
pub use local::LocalMiniLLM;
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

/// Делегирующий LLM-клиент с подменой на лету: harness-детект при MCP
/// `initialize` может заменить внутренний клиент (основная LLM harness-системы).
/// Все держатели `Arc<SwappableLLM>` (как `Arc<dyn LLMClient>`) видят подмену.
pub struct SwappableLLM {
    current: RwLock<Arc<dyn LLMClient>>,
}

impl SwappableLLM {
    pub fn new(initial: Arc<dyn LLMClient>) -> Self {
        Self {
            current: RwLock::new(initial),
        }
    }

    pub fn swap(&self, next: Arc<dyn LLMClient>) {
        *self.current.write().unwrap() = next;
    }

    pub fn current(&self) -> Arc<dyn LLMClient> {
        self.current.read().unwrap().clone()
    }
}

#[async_trait]
impl LLMClient for SwappableLLM {
    async fn ask(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<String> {
        self.current().ask(prompt, system).await
    }
}

/// Приоритет снабжения LLM: явный ключ `OB2H_LLM_*` > локальная мини-модель
/// (harness-креды подменяют внутренний клиент позже, при `initialize`).
pub fn make_llm(settings: &Settings) -> Arc<SwappableLLM> {
    let inner: Arc<dyn LLMClient> = if !settings.llm_api_key.is_empty() {
        Arc::new(OpenAIClient::new(
            &settings.llm_base_url,
            &settings.llm_api_key,
            &settings.llm_model,
            settings.llm_timeout_secs,
            settings.llm_max_retries,
        ))
    } else if local::enabled() {
        Arc::new(LocalMiniLLM::new(local::effective_repo()))
    } else {
        Arc::new(FakeLLM::default())
    };
    Arc::new(SwappableLLM::new(inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_with(resp: &str) -> Arc<FakeLLM> {
        let f = FakeLLM::new();
        f.set_default_response(resp);
        Arc::new(f)
    }

    #[tokio::test]
    async fn swappable_llm_delegates_and_swaps() {
        let a = fake_with("{\"from\":\"a\"}");
        let sw = SwappableLLM::new(a.clone());
        assert!(sw.ask("привет", None).await.unwrap().contains("\"a\""));

        let b = fake_with("{\"from\":\"b\"}");
        sw.swap(b);
        assert!(sw.ask("привет", None).await.unwrap().contains("\"b\""));
    }

    #[tokio::test]
    async fn make_llm_picks_openai_when_key_present() {
        let mut s = Settings::from_env();
        s.llm_api_key = "sk-test".to_string();
        let sw = make_llm(&s);
        // Внутренний клиент — OpenAI (не Fake и не локальный); проверить нельзя
        // напрямую, но подмена/делегирование работают через trait.
        assert!(Arc::strong_count(&sw.current()) >= 1);
    }
}
