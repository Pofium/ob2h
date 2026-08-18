//! Векторная математика и сериализация (BLOB float32, cosine, RRF).

pub mod rrf;
pub mod similarity;

pub use rrf::{rrf_merge, RankedItem};
pub use similarity::{cosine, deserialize, normalize, serialize, top_k};
