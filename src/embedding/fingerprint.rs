//! Механизм `EmbeddingFingerprint` (Ф72.3): изоляция векторных пространств.
//! Предотвращает смешивание несовместимых векторов при смене модели, бэкенда,
//! квантования, размерности, версии префиксов (72.2b) или чанкера (72.2a).

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::config::Settings;
use crate::db::Database;
use crate::embedding::onnx_spec::PREFIX_SCHEMA_VERSION;
use crate::project::chunker::CHUNKER_VERSION;

/// Ключ в таблице `kv` SQLite.
pub const KV_EMBEDDING_FINGERPRINT: &str = "embedding_fingerprint";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    pub model_id: String,
    pub backend: String,
    pub quantization: String,
    pub dimension: usize,
    pub prefix_version: String,
    pub chunker_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintStatus {
    /// В БД ещё нет сохранённого отпечатка (первый запуск / чистая БД).
    Fresh,
    /// Отпечаток совпадает с текущим.
    Match,
    /// Несовпадение — смешанный векторный поиск заблокирован, требуется `ob2h reindex --embeddings`.
    Mismatch { stored: String, current: String },
}

impl EmbeddingFingerprint {
    pub fn new(
        model_id: impl Into<String>,
        backend: impl Into<String>,
        quantization: impl Into<String>,
        dimension: usize,
        prefix_version: impl Into<String>,
        chunker_version: impl Into<String>,
    ) -> Self {
        Self {
            model_id: model_id.into().replace(':', "_"),
            backend: backend.into().replace(':', "_"),
            quantization: quantization.into().replace(':', "_"),
            dimension,
            prefix_version: prefix_version.into().replace(':', "_"),
            chunker_version: chunker_version.into().replace(':', "_"),
        }
    }

    /// Построить отпечаток непосредственно из `EmbeddingProvider`.
    pub fn from_provider(provider: &dyn crate::embedding::EmbeddingProvider) -> Self {
        let backend = provider.backend_id();
        let quant = if backend == "onnx" { "int8" } else { "fp32" };
        Self::new(
            provider.model_id(),
            backend,
            quant,
            provider.dim(),
            PREFIX_SCHEMA_VERSION,
            CHUNKER_VERSION,
        )
    }

    /// Построить отпечаток из текущего конфига, активного бэкенда и размерности.
    pub fn from_settings(settings: &Settings, backend: &str, dim: usize) -> Self {
        let model = if settings.embed_model.trim().is_empty() || settings.embed_model == "local" {
            if backend == "onnx" {
                "multilingual-e5-small"
            } else {
                "paraphrase-multilingual-MiniLM-L12-v2"
            }
        } else {
            settings.embed_model.as_str()
        };
        let quant = if backend == "onnx" { "int8" } else { "fp32" };
        Self::new(
            model,
            backend,
            quant,
            dim,
            PREFIX_SCHEMA_VERSION,
            CHUNKER_VERSION,
        )
    }

    pub fn as_key_string(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.model_id,
            self.backend,
            self.quantization,
            self.dimension,
            self.prefix_version,
            self.chunker_version
        )
    }
}

impl fmt::Display for EmbeddingFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_key_string())
    }
}

/// Проверить отпечаток в БД относительно текущего провайдера.
pub fn check_fingerprint(
    db: &Database,
    current: &EmbeddingFingerprint,
) -> anyhow::Result<FingerprintStatus> {
    let stored = db.get_kv(KV_EMBEDDING_FINGERPRINT)?;
    match stored {
        None => Ok(FingerprintStatus::Fresh),
        Some(s) if s.trim().is_empty() => Ok(FingerprintStatus::Fresh),
        Some(s) => {
            let cur = current.as_key_string();
            if s == cur {
                Ok(FingerprintStatus::Match)
            } else {
                Ok(FingerprintStatus::Mismatch {
                    stored: s,
                    current: cur,
                })
            }
        }
    }
}

/// Зафиксировать отпечаток в БД (после первой записи или после `reindex --embeddings`).
pub fn save_fingerprint(db: &Database, fp: &EmbeddingFingerprint) -> anyhow::Result<()> {
    db.set_kv(KV_EMBEDDING_FINGERPRINT, &fp.as_key_string())
}

/// Гарантировать, что при `Fresh` отпечаток сохраняется, а при `Mismatch` возвращается ошибка.
pub fn ensure_compatible_or_init(
    db: &Database,
    current: &EmbeddingFingerprint,
) -> anyhow::Result<()> {
    match check_fingerprint(db, current)? {
        FingerprintStatus::Fresh => {
            save_fingerprint(db, current)?;
            Ok(())
        }
        FingerprintStatus::Match => Ok(()),
        FingerprintStatus::Mismatch { stored, current } => {
            anyhow::bail!(
                "EmbeddingFingerprint mismatch: stored='{stored}', current='{current}'. Запустите `ob2h reindex --embeddings`"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_mismatch_on_model_or_chunker_or_prefix_change() {
        let db = Database::in_memory().unwrap();
        let fp1 = EmbeddingFingerprint::new(
            "multilingual-e5-small",
            "onnx",
            "int8",
            384,
            "prefix_v1",
            "ast_v1",
        );

        assert_eq!(check_fingerprint(&db, &fp1).unwrap(), FingerprintStatus::Fresh);
        ensure_compatible_or_init(&db, &fp1).unwrap();
        assert_eq!(check_fingerprint(&db, &fp1).unwrap(), FingerprintStatus::Match);

        // Смена модели -> Mismatch -> требуется reindex
        let fp_model = EmbeddingFingerprint::new(
            "jina-embeddings-v2-base-code",
            "onnx",
            "int8",
            768,
            "prefix_v1",
            "ast_v1",
        );
        assert!(matches!(
            check_fingerprint(&db, &fp_model).unwrap(),
            FingerprintStatus::Mismatch { .. }
        ));
        assert!(ensure_compatible_or_init(&db, &fp_model).is_err());

        // Смена чанкера (72.2a) -> Mismatch -> требуется reindex
        let fp_chunker = EmbeddingFingerprint::new(
            "multilingual-e5-small",
            "onnx",
            "int8",
            384,
            "prefix_v1",
            "ast_v2",
        );
        assert!(matches!(
            check_fingerprint(&db, &fp_chunker).unwrap(),
            FingerprintStatus::Mismatch { .. }
        ));

        // Смена префиксов (72.2b) -> Mismatch -> требуется reindex
        let fp_prefix = EmbeddingFingerprint::new(
            "multilingual-e5-small",
            "onnx",
            "int8",
            384,
            "prefix_v2",
            "ast_v1",
        );
        assert!(matches!(
            check_fingerprint(&db, &fp_prefix).unwrap(),
            FingerprintStatus::Mismatch { .. }
        ));
    }
}
