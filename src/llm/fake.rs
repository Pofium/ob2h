//! FakeLLM для тестирования и детерминированных ответов.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use super::LLMClient;

#[derive(Clone, Default)]
pub struct FakeLLM {
    responses: Arc<Mutex<HashMap<String, String>>>,
    default_response: Arc<Mutex<String>>,
}

impl FakeLLM {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(HashMap::new())),
            default_response: Arc::new(Mutex::new("{}".to_string())),
        }
    }

    pub fn set_response(&self, trigger_substr: &str, response: &str) {
        let mut map = self.responses.lock().unwrap();
        map.insert(trigger_substr.to_string(), response.to_string());
    }

    pub fn set_default_response(&self, response: &str) {
        let mut def = self.default_response.lock().unwrap();
        *def = response.to_string();
    }
}

#[async_trait]
impl LLMClient for FakeLLM {
    async fn ask(&self, prompt: &str, _system: Option<&str>) -> anyhow::Result<String> {
        let map = self.responses.lock().unwrap();
        for (k, v) in map.iter() {
            if prompt.contains(k) {
                return Ok(v.clone());
            }
        }
        let def = self.default_response.lock().unwrap();
        Ok(def.clone())
    }
}
