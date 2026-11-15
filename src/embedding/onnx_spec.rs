//! Спецификация ONNX-моделей мультиязычного/кодового класса и Matryoshka-урезание (Ф72.2).
//! Вынесено из `fastembed_local.rs`, чтобы логика выбора моделей и тесты работали
//! в zero-dependency сборке без линковки `ort-sys`.

use tracing::warn;

use crate::vector::normalize;

/// Версия схемы префиксов (входит в `EmbeddingFingerprint`, Ф72.3).
pub const PREFIX_SCHEMA_VERSION: &str = "prefix_v1";

/// Асимметричные префиксы модели: `(query_prefix, document_prefix)` (Ф72.2b).
/// E5: `("query: ", "passage: ")`, Nomic: `("search_query: ", "search_document: ")`,
/// Candle MiniLM и jina-v2 — no-op `("", "")`.
pub fn prefixes_for_model(model_id: &str) -> (&'static str, &'static str) {
    let lower = model_id.trim().to_lowercase();
    if lower.contains("e5") {
        ("query: ", "passage: ")
    } else if lower.contains("nomic") {
        ("search_query: ", "search_document: ")
    } else {
        ("", "")
    }
}

/// Спецификация выбранной ONNX-модели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnnxModelSpec {
    pub canonical_id: &'static str,
    pub dim: usize,
    pub matryoshka_dim: Option<usize>,
}

/// Выбрать спецификацию модели по имени (Ф72.2).
/// Англоязычные `bge-small-en*` / `all-MiniLM*` отклоняются в пользу `multilingual-e5-small`.
pub fn resolve_onnx_model_spec(name: &str) -> OnnxModelSpec {
    let lower = name.trim().to_lowercase();
    if lower.contains("jina") && lower.contains("code") {
        OnnxModelSpec {
            canonical_id: "jina-embeddings-v2-base-code",
            dim: 768,
            matryoshka_dim: None,
        }
    } else if lower.contains("nomic") {
        let m_dim = if lower.contains("256") {
            Some(256)
        } else if lower.contains("384") {
            Some(384)
        } else {
            None
        };
        OnnxModelSpec {
            canonical_id: "nomic-embed-text-v1.5",
            dim: m_dim.unwrap_or(768),
            matryoshka_dim: m_dim,
        }
    } else {
        if lower.contains("bge-small-en") || lower.contains("all-minilm") {
            warn!(
                "Модель '{name}' — строго англоязычная и не проходит RU-гейт (Ф72.2); переключаемся на multilingual-e5-small"
            );
        }
        OnnxModelSpec {
            canonical_id: "multilingual-e5-small",
            dim: 384,
            matryoshka_dim: None,
        }
    }
}

/// Matryoshka-урезание (и повторная L2-нормализация) при заданной целевой размерности.
pub fn apply_matryoshka(mut vec: Vec<f32>, target_dim: Option<usize>) -> Vec<f32> {
    if let Some(d) = target_dim {
        if d > 0 && vec.len() > d {
            vec.truncate(d);
        }
    }
    normalize(&vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_onnx_model_spec_multilingual_default() {
        let s = resolve_onnx_model_spec("");
        assert_eq!(s.canonical_id, "multilingual-e5-small");
        assert_eq!(s.dim, 384);

        let rejected = resolve_onnx_model_spec("bge-small-en-v1.5");
        assert_eq!(rejected.canonical_id, "multilingual-e5-small");
    }

    #[test]
    fn test_resolve_onnx_model_spec_code_and_nomic_matryoshka() {
        let jina = resolve_onnx_model_spec("jina-embeddings-v2-base-code");
        assert_eq!(jina.canonical_id, "jina-embeddings-v2-base-code");
        assert_eq!(jina.dim, 768);

        let nomic = resolve_onnx_model_spec("nomic-embed-text-v1.5-384");
        assert_eq!(nomic.canonical_id, "nomic-embed-text-v1.5");
        assert_eq!(nomic.dim, 384);
        assert_eq!(nomic.matryoshka_dim, Some(384));
    }

    #[test]
    fn test_apply_matryoshka_truncates_and_normalizes() {
        let v = vec![3.0f32, 4.0, 100.0, 200.0];
        let out = apply_matryoshka(v, Some(2));
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.6).abs() < 1e-5);
        assert!((out[1] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn test_prefixes_for_model() {
        assert_eq!(
            prefixes_for_model("intfloat/multilingual-e5-small"),
            ("query: ", "passage: ")
        );
        assert_eq!(
            prefixes_for_model("nomic-embed-text-v1.5"),
            ("search_query: ", "search_document: ")
        );
        assert_eq!(
            prefixes_for_model("sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"),
            ("", "")
        );
        assert_eq!(
            prefixes_for_model("jina-embeddings-v2-base-code"),
            ("", "")
        );
    }
}
