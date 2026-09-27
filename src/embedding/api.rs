//! OpenAI-совместимый клиент API эмбеддингов.

use super::EmbeddingProvider;
use crate::vector::normalize;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

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

/// Лимит одного текста в батче, символов. Всё сверх обрезается: тело запроса
/// ограничено сверху, а эмбеддинг-модели всё равно режут вход по токенам.
const MAX_INPUT_CHARS: usize = 8000;

/// Валидация endpoint'а embedding API: только http(s), хост обязателен,
/// userinfo (логин/пароль в URL) запрещён. Базовый URL приходит из env-конфига
/// оператора — проверяем его явно, чтобы ошибка конфига была читаемой, а клиент
/// не мог отправить запрос по схеме вроде file/ftp.
fn validate_endpoint(raw: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw)
        .map_err(|e| anyhow::anyhow!("Некорректный URL embedding API `{raw}`: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => anyhow::bail!(
            "Embedding API: схема `{other}` не поддерживается (только http/https): {url}"
        ),
    }
    if url.host_str().is_none() {
        anyhow::bail!("Embedding API: URL без хоста: {url}");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("Embedding API: логин/пароль в URL не поддерживаются: {url}");
    }
    Ok(url)
}

/// Обрезка входов до MAX_INPUT_CHARS (порядок сохраняется, индексы ответа
/// остаются выровнены по входу).
fn clip_inputs(texts: &[String]) -> Vec<String> {
    texts
        .iter()
        .map(|t| t.chars().take(MAX_INPUT_CHARS).collect())
        .collect()
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
        let endpoint = validate_endpoint(&self.base_url)?;

        let owned: Vec<String>;
        let input: &[String] = if texts
            .iter()
            .any(|t| t.chars().nth(MAX_INPUT_CHARS).is_some())
        {
            owned = clip_inputs(texts);
            &owned
        } else {
            texts
        };

        let mut req = self.client.post(endpoint);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let body = EmbeddingRequest {
            input,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_accepts_http_https() {
        assert!(validate_endpoint("https://api.example.com/v1/embeddings").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:8080/v1/embeddings").is_ok());
    }

    #[test]
    fn endpoint_rejects_non_http_schemes() {
        for raw in [
            "ftp://api.example.com/embeddings",
            "file:///etc/passwd",
            "unix:/run/socket",
        ] {
            assert!(validate_endpoint(raw).is_err(), "must reject {raw}");
        }
    }

    #[test]
    fn endpoint_rejects_no_host_and_userinfo() {
        // "not a url" — относительный URL без базы → ошибка парсинга
        assert!(validate_endpoint("not a url").is_err());
        assert!(validate_endpoint("http://user:pass@api.example.com/e").is_err());
        assert!(validate_endpoint("http://user@api.example.com/e").is_err());
    }

    #[test]
    fn clip_inputs_truncates_and_preserves_order_count() {
        let long = "ж".repeat(MAX_INPUT_CHARS + 100);
        let out = clip_inputs(&[String::new(), "ok".into(), long]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], "");
        assert_eq!(out[1], "ok");
        assert_eq!(out[2].chars().count(), MAX_INPUT_CHARS);
    }

    #[test]
    fn clip_inputs_keeps_short_texts_intact() {
        let out = clip_inputs(&["привет мир".into()]);
        assert_eq!(out[0], "привет мир");
    }
}
