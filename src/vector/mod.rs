//! Векторная математика и сериализация (BLOB float32, cosine, RRF).

pub mod rrf;
pub mod similarity;

pub use rrf::{rrf_merge, RankedItem};
pub use similarity::{cosine, deserialize, deserialize_f32, deserialize_q, normalize, serialize, serialize_q, top_k};
