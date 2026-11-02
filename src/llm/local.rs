//! Локальная мини-модель для дриминга (Candle, архитектура Qwen3).
//!
//! Третий эшелон снабжения LLM: когда явный ключ `OB2H_LLM_*` не задан и
//! harness-детект не дал кредов. Модель качается с Hugging Face при первом
//! `ask` (тот же механизм `hf-hub`, что у эмбеддингов) — старт serve не
//! блокируется. Дефолт `Qwen/Qwen3-0.6B` (~1.4 GB, свежая официальная
//! суб-1B модель, мультиязычная).
//!
//! Управление: `OB2H_LOCAL_LLM=0` — выключить; `OB2H_LOCAL_LLM_MODEL=<repo>`
//! — другая модель (архитектура должна быть qwen3-совместимой).

use std::path::Path;

use anyhow::Context;
use async_trait::async_trait;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::qwen3::{Config as Qwen3Config, ModelForCausalLM};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;
use tracing::info;

use super::LLMClient;

pub const DEFAULT_REPO: &str = "Qwen/Qwen3-0.6B";
/// Лимит генерации на один запрос: дрим-промпты не нуждаются в длинных ответах.
const MAX_NEW_TOKENS: usize = 512;

pub struct LocalMiniLLM {
    repo: String,
    /// Ленивая инициализация: скачивание/загрузка весов при первом ask.
    runtime: std::sync::Mutex<Option<Runtime>>,
}

struct Runtime {
    tokenizer: Tokenizer,
    model: ModelForCausalLM,
    stop_ids: Vec<u32>,
}

/// Скачивание (при отсутствии в кэше hf-hub) и загрузка весов в память.
/// Свободная функция, чтобы не конфликтовать с MutexGuard по заимствованиям.
fn load_model(repo_id: &str) -> anyhow::Result<Runtime> {
    info!(
        "Локальная мини-модель: инициализация {repo_id} (при первом запуске может \
         скачаться ~1.4 GB с Hugging Face)"
    );
    let api = Api::new().context("hf-hub Api::new")?;
    let repo = api.repo(Repo::new(repo_id.to_string(), RepoType::Model));
    let weights = repo
        .get("model.safetensors")
        .with_context(|| format!("скачивание весов {repo_id}"))?;
    let tokenizer_path = repo.get("tokenizer.json")?;
    let config_path = repo.get("config.json")?;

    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| anyhow::anyhow!("tokenizer.json: {e}"))?;
    let cfg: Qwen3Config = serde_json::from_str(&std::fs::read_to_string(&config_path)?)
        .context("config.json → qwen3::Config")?;
    let device = Device::Cpu;
    // F32 — как в local_bert: предсказуемая точность на CPU (~2.4 GB RAM для 0.6B).
    let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device)? };
    let model = ModelForCausalLM::new(&cfg, vb)?;

    let mut stop_ids = Vec::new();
    for marker in ["<|im_end|>", "<|endoftext|>"] {
        if let Some(id) = tokenizer.token_to_id(marker) {
            stop_ids.push(id);
        }
    }
    info!("Локальная мини-модель {repo_id} готова (CPU, F32)");
    Ok(Runtime {
        tokenizer,
        model,
        stop_ids,
    })
}

impl LocalMiniLLM {
    pub fn new(repo: String) -> Self {
        Self {
            repo,
            runtime: std::sync::Mutex::new(None),
        }
    }

    fn ask_sync(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<String> {
        let mut guard = self.runtime.lock().unwrap();
        if guard.is_none() {
            *guard = Some(load_model(&self.repo)?);
        }
        let rt = guard.as_mut().unwrap();

        let chat = build_chat_prompt(system.unwrap_or(""), prompt);
        let mut last: Vec<u32> = rt
            .tokenizer
            .encode(chat, false)
            .map_err(|e| anyhow::anyhow!("encode: {e}"))?
            .get_ids()
            .to_vec();
        if last.is_empty() {
            anyhow::bail!("пустой промпт после токенизации");
        }

        let device = Device::Cpu;
        let mut generated: Vec<u32> = Vec::new();
        let mut offset = 0usize;
        rt.model.clear_kv_cache();

        for _ in 0..MAX_NEW_TOKENS {
            let input = Tensor::new(&last[..], &device)?.unsqueeze(0)?;
            let logits = rt.model.forward(&input, offset)?;
            offset += last.len();
            let next = logits.squeeze(0)?.squeeze(0)?.argmax(candle_core::D::Minus1)?;
            let token = next.to_scalar::<u32>()?;
            if rt.stop_ids.contains(&token) {
                break;
            }
            generated.push(token);
            last = vec![token];
        }

        let text = rt
            .tokenizer
            .decode(&generated, true)
            .map_err(|e| anyhow::anyhow!("decode: {e}"))?;
        Ok(strip_think(&text).trim().to_string())
    }
}

#[async_trait]
impl LLMClient for LocalMiniLLM {
    async fn ask(&self, prompt: &str, system: Option<&str>) -> anyhow::Result<String> {
        // CPU-генерация блокирующая — выносим из async-рутины.
        let this = self;
        tokio::task::block_in_place(|| this.ask_sync(prompt, system))
    }
}

/// Включена ли локальная модель (по умолчанию да; `OB2H_LOCAL_LLM=0` — нет).
pub fn enabled() -> bool {
    std::env::var("OB2H_LOCAL_LLM")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true)
}

/// Репозиторий модели из env или дефолт.
pub fn effective_repo() -> String {
    std::env::var("OB2H_LOCAL_LLM_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

/// Qwen-чат-разметка + мягкое отключение «мышления» (`/no_think`):
/// дримингу нужны короткие ответы, а не рассуждения.
pub fn build_chat_prompt(system: &str, user: &str) -> String {
    let sys = if system.trim().is_empty() {
        "/no_think".to_string()
    } else {
        format!("{}\n/no_think", system.trim())
    };
    format!(
        "<|im_start|>system\n{sys}<|im_end|>\n<|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n"
    )
}

/// Вырезать блок рассуждений `<think>…</think>`, если модель его выдала.
pub fn strip_think(text: &str) -> &str {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            return rest[end + "</think>".len()..].trim_start();
        }
    }
    t
}

/// Путь кэша hf-hub для диагностики (не обязателен для работы).
pub fn hf_cache_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HF_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|h| Path::new(&h).join(".cache").join("huggingface"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_prompt_qwen_markup_and_no_think() {
        let p = build_chat_prompt("Ты экстрактор сущностей.", "Иван основал OmnesCorp.");
        assert!(p.starts_with("<|im_start|>system\n"));
        assert!(p.contains("/no_think"));
        assert!(p.ends_with("<|im_start|>assistant\n"));
        let p2 = build_chat_prompt("", "вопрос");
        assert!(p2.starts_with("<|im_start|>system\n/no_think<|im_end|>"));
    }

    #[test]
    fn strip_think_removes_reasoning_block() {
        assert_eq!(strip_think("<think>размышления</think>Ответ"), "Ответ");
        assert_eq!(strip_think("Просто ответ"), "Просто ответ");
        assert_eq!(strip_think("  <think>x</think>\n\nДальше"), "Дальше");
    }

    #[test]
    fn enabled_and_repo_overrides() {
        std::env::set_var("OB2H_LOCAL_LLM_MODEL", "Qwen/Qwen3-1.7B");
        assert_eq!(effective_repo(), "Qwen/Qwen3-1.7B");
        std::env::remove_var("OB2H_LOCAL_LLM_MODEL");
        assert_eq!(effective_repo(), DEFAULT_REPO);

        std::env::set_var("OB2H_LOCAL_LLM", "0");
        assert!(!enabled());
        std::env::set_var("OB2H_LOCAL_LLM", "1");
        assert!(enabled());
        std::env::remove_var("OB2H_LOCAL_LLM");
        assert!(enabled());
    }

    /// Реальный инференс — только при наличии модели в кэше hf-hub
    /// (аналог gated-тестов эмбеддингов: офлайн по умолчанию).
    #[test]
    #[ignore = "требует сети/кэша hf-hub (~1.4 GB): cargo test -- --ignored"]
    fn real_generation_smoke() {
        std::env::set_var("OB2H_LOCAL_LLM", "1");
        let llm = LocalMiniLLM::new(effective_repo());
        let out = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(llm.ask("Назови столицу Франции одним словом.", None))
            .expect("локальная генерация");
        assert!(!out.trim().is_empty());
        assert!(!out.contains("<think>"));
    }
}
