//! Сериализация BLOB float32 и косинусный поиск перебором (ADR-2).

use bytemuck::{cast_slice, try_cast_slice};

/// Сериализация вектора f32 в бинарный BLOB (little-endian).
pub fn serialize(vec: &[f32]) -> Vec<u8> {
    cast_slice(vec).to_vec()
}

/// Десериализация бинарного BLOB в вектор f32.
pub fn deserialize(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.is_empty() || blob.len() % std::mem::size_of::<f32>() != 0 {
        return None;
    }
    match try_cast_slice(blob) {
        Ok(slice) => Some(slice.to_vec()),
        Err(_) => {
            // Если выравнивание не подошло, копируем через chunks
            let floats: Vec<f32> = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            Some(floats)
        }
    }
}

/// Нормализация вектора L2.
pub fn normalize(vec: &[f32]) -> Vec<f32> {
    let norm_sq: f32 = vec.iter().map(|v| v * v).sum();
    let norm = norm_sq.sqrt();
    if norm == 0.0 {
        return vec.to_vec();
    }
    vec.iter().map(|v| v / norm).collect()
}

/// Косинусное сходство между двумя векторами.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a_sq = 0.0f32;
    let mut norm_b_sq = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a_sq += x * x;
        norm_b_sq += y * y;
    }

    let denom = norm_a_sq.sqrt() * norm_b_sq.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Поиск top_k ближайших кандидатов по косинусному сходству.
pub fn top_k(
    query: &[f32],
    candidates: &[(i64, Option<&[u8]>)],
    k: usize,
    min_score: f32,
) -> Vec<(i64, f32)> {
    let mut scored: Vec<(i64, f32)> = Vec::new();

    for &(id, blob_opt) in candidates {
        if let Some(blob) = blob_opt {
            if let Some(vec) = deserialize(blob) {
                if vec.len() == query.len() {
                    let score = cosine(query, &vec);
                    if score >= min_score {
                        scored.push((id, score));
                    }
                }
            }
        }
    }

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if scored.len() > k {
        scored.truncate(k);
    }
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_deserialize() {
        let original = vec![0.1f32, -0.5, 1.25, 0.0];
        let bytes = serialize(&original);
        assert_eq!(bytes.len(), 16);
        let restored = deserialize(&bytes).expect("must deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn test_cosine() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![1.0f32, 0.0, 0.0];
        let c = vec![0.0f32, 1.0, 0.0];
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-5);
        assert!((cosine(&a, &c) - 0.0).abs() < 1e-5);
    }
}
