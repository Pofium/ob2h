//! Автоподхват основной LLM harness-системы, к которой подключен ob2h.
//!
//! При MCP `initialize` клиент сообщает своё имя (`clientInfo.name`); по нему
//! выбирается ридер конфига harness. Возвращённые base_url/ключ/модель
//! подменяют LLM-клиент ob2h (`SwappableLLM::swap`). Приоритет снабжения LLM:
//! явный `OB2H_LLM_*` > harness > локальная мини-модель (`llm::local`).
//!
//! Поддержка: hermes (config.yaml), zcode/omnesagent (config.toml), codex
//! (auth.json). Claude/Trae/Qoder/DSH — account/OAuth-auth, статических ключей
//! в доступных конфигах нет → остаются на локальной мини-модели.
//! Форс конкретной harness: env `OB2H_LLM_HARNESS=<имя>`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::info;

use crate::config::read_env_file_value;

/// Основная LLM harness-системы, извлечённая из её конфига.
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessLlm {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Человекочитаемый источник («hermes config.yaml») для логов.
    pub source: String,
}

/// Класс harness по имени MCP-клиента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessKind {
    Hermes,
    OmnesAgent,
    Codex,
    /// Account/OAuth-auth: статического ключа в доступных конфигах нет.
    AccountAuth,
    Unknown,
}

/// Класс harness с учётом форса `OB2H_LLM_HARNESS` (переопределяет clientInfo).
pub fn kind_of(client_name: &str) -> HarnessKind {
    let forced = std::env::var("OB2H_LLM_HARNESS").unwrap_or_default();
    let name = if forced.trim().is_empty() {
        client_name.to_lowercase()
    } else {
        forced.trim().to_lowercase()
    };
    if name.contains("hermes") {
        HarnessKind::Hermes
    } else if name.contains("zcode") || name.contains("omnesagent") {
        HarnessKind::OmnesAgent
    } else if name.contains("codex") {
        HarnessKind::Codex
    } else if name.contains("claude")
        || name.contains("trae")
        || name.contains("qoder")
        || name.contains("dsh")
    {
        HarnessKind::AccountAuth
    } else {
        HarnessKind::Unknown
    }
}

/// Определить harness по имени клиента MCP и вытащить из её конфига основную
/// LLM. None — ридера нет или конфиг не дал рабочего ключа (причина логируется).
pub fn detect(client_name: &str, data_dir: &Path) -> Option<HarnessLlm> {
    let llm = match kind_of(client_name) {
        HarnessKind::Hermes => {
            let cfg = localappdata()?.join("hermes").join("config.yaml");
            read_hermes(&cfg, data_dir)
        }
        HarnessKind::OmnesAgent => {
            let cfg = home_dir()?.join(".omnesagent").join("config.toml");
            read_omnesagent(&cfg, data_dir)
        }
        HarnessKind::Codex => read_codex(&home_dir()?),
        HarnessKind::AccountAuth => {
            info!(
                "Harness '{client_name}': account/OAuth-auth — статического ключа нет, \
                 остаётся локальная мини-модель"
            );
            None
        }
        HarnessKind::Unknown => None,
    };

    if let Some(h) = &llm {
        info!(
            "Harness '{client_name}': основная LLM найдена — model={}, base_url={} ({})",
            h.model, h.base_url, h.source
        );
    }
    llm
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn localappdata() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join("AppData").join("Local")))
}

/// Ключ провайдера: поле `api_key` либо `key_env` (env + фолбэк на .env рядом
/// с конфигом и в data_dir — cwd у serve-процесса не обязан совпадать).
fn resolve_key(
    api_key: &Option<String>,
    key_env: &Option<String>,
    data_dir: &Path,
    cfg_dir: &Path,
) -> Option<String> {
    if let Some(k) = api_key {
        if !k.trim().is_empty() {
            return Some(k.clone());
        }
    }
    if let Some(name) = key_env {
        if let Ok(v) = std::env::var(name) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
        if let Some(v) = read_env_file_value(&cfg_dir.join(".env"), name) {
            return Some(v);
        }
        if let Some(v) = read_env_file_value(&data_dir.join(".env"), name) {
            return Some(v);
        }
    }
    None
}

// --- Hermes: %LOCALAPPDATA%/hermes/config.yaml ---

#[derive(Deserialize, Default)]
struct HermesConfig {
    #[serde(default)]
    model: HermesModel,
    #[serde(default)]
    providers: BTreeMap<String, HermesProvider>,
}

#[derive(Deserialize, Default)]
struct HermesModel {
    #[serde(default)]
    default: String,
    #[serde(default)]
    provider: String,
}

#[derive(Deserialize)]
struct HermesProvider {
    #[serde(default)]
    base_url: Option<String>,
    /// Поле `api` в конфиге Hermes — это base_url провайдера.
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    key_env: Option<String>,
}

fn read_hermes(cfg_path: &Path, data_dir: &Path) -> Option<HarnessLlm> {
    let raw = std::fs::read_to_string(cfg_path).ok()?;
    let cfg: HermesConfig = serde_yaml_ng::from_str(&raw).ok()?;

    let provider_id = cfg
        .model
        .provider
        .strip_prefix("custom:")
        .unwrap_or(&cfg.model.provider);
    if provider_id.is_empty() {
        return None;
    }
    let p = cfg.providers.get(provider_id)?;
    let base_url = p
        .base_url
        .clone()
        .or_else(|| p.api.clone())
        .filter(|u| !u.trim().is_empty())?;
    let cfg_dir = cfg_path.parent()?;
    let api_key = resolve_key(&p.api_key, &p.key_env, data_dir, cfg_dir)?;
    let model = if cfg.model.default.is_empty() {
        p.model.clone().unwrap_or_default()
    } else {
        cfg.model.default.clone()
    };
    if model.is_empty() {
        return None;
    }
    Some(HarnessLlm {
        base_url,
        api_key,
        model,
        source: "hermes config.yaml".to_string(),
    })
}

// --- ZCode/OmnesAgent: ~/.omnesagent/config.toml ---

/// Эндпоинты встроенных провайдеров omnesagent (полей base_url в конфиге нет).
/// Anthropic намеренно отсутствует: у него другой протокол (/v1/messages).
fn builtin_base_url(provider_name: &str) -> Option<&'static str> {
    match provider_name {
        "deepseek" => Some("https://api.deepseek.com/v1"),
        "openrouter" => Some("https://openrouter.ai/api/v1"),
        _ => None,
    }
}

#[derive(Deserialize, Default)]
struct OmnesConfig {
    #[serde(default)]
    providers: OmnesProviders,
    #[serde(default)]
    agents: BTreeMap<String, OmnesAgent>,
}

#[derive(Deserialize, Default)]
struct OmnesProviders {
    #[serde(default, rename = "models")]
    models: BTreeMap<String, OmnesProviderGroup>,
}

/// Группа провайдера: `[providers.models.deepseek.default]` — ключ "default",
/// вложенность может быть глубже (anthropic.anthropic) — берём flatten-карту.
#[derive(Deserialize)]
struct OmnesProviderGroup {
    #[serde(flatten)]
    entries: BTreeMap<String, OmnesProviderEntry>,
}

#[derive(Deserialize, Default)]
struct OmnesProviderEntry {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    key_env: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api: Option<String>,
}

#[derive(Deserialize)]
struct OmnesAgent {
    #[serde(default, rename = "model_provider")]
    model_provider: Option<String>,
}

fn read_omnesagent(cfg_path: &Path, data_dir: &Path) -> Option<HarnessLlm> {
    let raw = std::fs::read_to_string(cfg_path).ok()?;
    let cfg: OmnesConfig = toml::from_str(&raw).ok()?;

    // Приоритет: провайдер активного агента, если он OpenAI-совместим.
    let active = cfg.agents.values().find_map(|a| a.model_provider.clone());
    let try_provider = |provider_key: &str| -> Option<HarnessLlm> {
        let (group_name, entry_name) = match provider_key.split_once('.') {
            Some((g, e)) => (g, e),
            None => (provider_key, "default"),
        };
        let group = cfg.providers.models.get(group_name)?;
        let entry = group.entries.get(entry_name)?;
        let base_url = entry
            .base_url
            .clone()
            .or_else(|| entry.api.clone())
            .or_else(|| builtin_base_url(group_name).map(String::from))?;
        let cfg_dir = cfg_path.parent()?;
        let api_key = resolve_key(&entry.api_key, &entry.key_env, data_dir, cfg_dir)?;
        Some(HarnessLlm {
            base_url,
            api_key,
            model: entry.model.clone().unwrap_or_default(),
            source: "omnesagent config.toml".to_string(),
        })
    };

    if let Some(active_key) = &active {
        match try_provider(active_key) {
            Some(llm) => return Some(llm),
            None => info!(
                "omnesagent: активный провайдер '{active_key}' не OpenAI-совместим \
                 (anthropic использует другой протокол) — берём совместимый из конфига"
            ),
        }
    }

    // Фолбэк: первый совместимый провайдер с ключом (BTreeMap — алфавитный порядок).
    for group_name in cfg.providers.models.keys() {
        if let Some(llm) = try_provider(&format!("{group_name}.default")) {
            return Some(llm);
        }
    }
    None
}

// --- Codex: ~/.codex/auth.json (API-key mode) ---

fn read_codex(home: &Path) -> Option<HarnessLlm> {
    let raw = std::fs::read_to_string(home.join(".codex").join("auth.json")).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let key = auth
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if key.trim().is_empty() {
        info!(
            "Codex: ChatGPT-mode (auth.json без OPENAI_API_KEY) — ключа нет, \
             остаётся локальная мини-модель"
        );
        return None;
    }
    let model = std::fs::read_to_string(home.join(".codex").join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| v.get("model").and_then(|m| m.as_str().map(String::from)))
        .unwrap_or_default();
    Some(HarnessLlm {
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: key,
        model,
        source: "codex auth.json".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Сериализация тестов, трогающих глобальный env (cargo-тесты параллельны).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Ридеры принимают явные пути — глобальный env в тестах не трогаем
    // (тесты cargo идут параллельно, мутация env = гонки).

    #[test]
    fn hermes_yaml_provider_with_key_env() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("hermes").join("config.yaml");
        write(
            &cfg,
            "model:\n  default: deepseek-v4.1-flash\n  provider: custom:daluobo\n\
             providers:\n  daluobo:\n    api: https://api.daluobo.cc.cd/v1\n    key_env: DALUOBO_TEST_KEY\n    models: {}\n",
        );
        // Ключ в .env рядом с конфигом (env процесса не трогаем).
        write(&cfg.parent().unwrap().join(".env"), "DALUOBO_TEST_KEY=sk-daluobo\n");
        let h = read_hermes(&cfg, tmp.path()).expect("hermes llm");
        assert_eq!(h.base_url, "https://api.daluobo.cc.cd/v1");
        assert_eq!(h.api_key, "sk-daluobo");
        assert_eq!(h.model, "deepseek-v4.1-flash");
    }

    #[test]
    fn hermes_yaml_literal_api_key_and_base_url_field() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("hermes").join("config.yaml");
        write(
            &cfg,
            "model:\n  default: m1\n  provider: p\n\
             providers:\n  p:\n    base_url: http://127.0.0.1:9/v1\n    api_key: sk-literal\n",
        );
        let h = read_hermes(&cfg, tmp.path()).expect("hermes llm");
        assert_eq!(h.base_url, "http://127.0.0.1:9/v1");
        assert_eq!(h.api_key, "sk-literal");
    }

    #[test]
    fn hermes_key_env_falls_back_to_data_dir_env() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("hermes").join("config.yaml");
        write(
            &cfg,
            "model:\n  default: m1\n  provider: p\nproviders:\n  p:\n    api: http://x/v1\n    key_env: HERMES_FALLBACK_TEST\n",
        );
        write(&tmp.path().join(".env"), "HERMES_FALLBACK_TEST=sk-from-data\n");
        let h = read_hermes(&cfg, tmp.path()).expect("hermes llm");
        assert_eq!(h.api_key, "sk-from-data");
    }

    #[test]
    fn omnesagent_active_anthropic_falls_back_to_compatible() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".omnesagent").join("config.toml");
        write(
            &cfg,
            "[providers.models.deepseek.default]\napi_key = \"\"\nkey_env = \"OMNES_TEST_DS_KEY\"\nmodel = \"deepseek-chat\"\n\n\
             [providers.models.openrouter.default]\napi_key = \"sk-or\"\nmodel = \"google/gemini-2.5-flash\"\n\n\
             [providers.models.anthropic]\n\n\
             [providers.models.anthropic.anthropic]\nmodel = \"claude-sonnet-4-5\"\napi_key = \"sk-ant\"\n\n\
             [agents.omnes]\nmodel_provider = \"anthropic.anthropic\"\n",
        );
        write(
            &cfg.parent().unwrap().join(".env"),
            "OMNES_TEST_DS_KEY=sk-ds\n",
        );
        let h = read_omnesagent(&cfg, tmp.path()).expect("omnesagent llm");
        // Активный anthropic пропущен (нет встроенного эндпоинта) → первый совместимый deepseek.
        assert_eq!(h.base_url, "https://api.deepseek.com/v1");
        assert_eq!(h.api_key, "sk-ds");
        assert_eq!(h.model, "deepseek-chat");
    }

    #[test]
    fn omnesagent_active_compatible_provider_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".omnesagent").join("config.toml");
        write(
            &cfg,
            "[providers.models.deepseek.default]\napi_key = \"sk-ds\"\nmodel = \"deepseek-chat\"\n\n\
             [providers.models.openrouter.default]\napi_key = \"sk-or\"\nmodel = \"google/gemini-2.5-flash\"\n\n\
             [agents.omnes]\nmodel_provider = \"openrouter.default\"\n",
        );
        let h = read_omnesagent(&cfg, tmp.path()).expect("omnesagent llm");
        assert_eq!(h.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(h.api_key, "sk-or");
    }

    #[test]
    fn codex_api_key_mode_and_chatgpt_mode() {
        let tmp = tempfile::tempdir().unwrap();

        // ChatGPT-mode: auth.json без OPENAI_API_KEY → None.
        write(&tmp.path().join(".codex/auth.json"), "{\"tokens\":{\"id_token\":\"x\"}}");
        assert!(read_codex(tmp.path()).is_none());

        // API-key mode.
        write(&tmp.path().join(".codex/auth.json"), "{\"OPENAI_API_KEY\":\"sk-oai\"}");
        write(&tmp.path().join(".codex/config.toml"), "model = \"gpt-5.2\"\n");
        let h = read_codex(tmp.path()).expect("codex llm");
        assert_eq!(h.base_url, "https://api.openai.com/v1");
        assert_eq!(h.model, "gpt-5.2");
    }

    #[test]
    fn kind_of_maps_client_names() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("OB2H_LLM_HARNESS");
        assert_eq!(kind_of("hermes"), HarnessKind::Hermes);
        assert_eq!(kind_of("Hermes"), HarnessKind::Hermes);
        assert_eq!(kind_of("zcode"), HarnessKind::OmnesAgent);
        assert_eq!(kind_of("omnesagent"), HarnessKind::OmnesAgent);
        assert_eq!(kind_of("codex"), HarnessKind::Codex);
        for name in ["claude-code", "trae", "qoder", "dsh"] {
            assert_eq!(kind_of(name), HarnessKind::AccountAuth, "{name}");
        }
        assert_eq!(kind_of("совершенно-неизвестный"), HarnessKind::Unknown);
    }

    #[test]
    fn forced_harness_env_overrides_client_name() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("OB2H_LLM_HARNESS", "hermes");
        // Клиент неизвестен, но OB2H_LLM_HARNESS форсирует hermes-класс.
        assert_eq!(kind_of("совершенно-неизвестный-клиент"), HarnessKind::Hermes);
        std::env::remove_var("OB2H_LLM_HARNESS");
        assert_eq!(
            kind_of("совершенно-неизвестный-клиент"),
            HarnessKind::Unknown
        );
    }
}
