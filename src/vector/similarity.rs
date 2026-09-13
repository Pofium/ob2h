//! Сериализация BLOB (f32 legacy + int8 v2), косинусный поиск перебором (ADR-2).

use bytemuck::{cast_slice, try_cast_slice};

/// Магический байт формата v2: [0x01][scale f32 LE][i8 × dim] (Ф24 PLAN_v1.3).
/// Длина v2 = dim+5 — никогда не кратна 4 для реальных размерностей (384→389),
/// но различаем по magic, а не только по длине.
pub const Q_MAGIC: u8 = 0x01;

/// Сериализация вектора f32 в бинарный BLOB (little-endian). Легаси-формат;
/// новые записи должны использовать serialize_q (int8, ~4× компактнее).
pub fn serialize(vec: &[f32]) -> Vec<u8> {
    cast_slice(vec).to_vec()
}

/// Квантование int8 per-vector scale: scale = max|v|/127, i8 = round(v/scale).
pub fn serialize_q(vec: &[f32]) -> Vec<u8> {
    let max_abs = vec.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let scale = if max_abs == 0.0 { 1.0 } else { max_abs / 127.0 };
    let mut out = Vec::with_capacity(vec.len() + 5);
    out.push(Q_MAGIC);
    out.extend_from_slice(&scale.to_le_bytes());
    for v in vec {
        let q = (v / scale).round().clamp(-127.0, 127.0) as i8;
        out.push(q as u8);
    }
    out
}

/// Десериализация легаси f32 BLOB.
pub fn deserialize_f32(blob: &[u8]) -> Option<Vec<f32>> {
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

/// Десериализация v2 (int8 + scale).
pub fn deserialize_q(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() < 5 || blob[0] != Q_MAGIC {
        return None;
    }
    let scale = f32::from_le_bytes(blob[1..5].try_into().ok()?);
    Some(blob[5..].iter().map(|&b| (b as i8) as f32 * scale).collect())
}

/// Dual-read: v2 по magic-байту, иначе легаси f32.
pub fn deserialize(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.first() == Some(&Q_MAGIC) {
        deserialize_q(blob)
    } else {
        deserialize_f32(blob)
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
    fn test_quantize_roundtrip_distortion() {
        // Ф24: искажение косинуса при int8-квантовании ≤ 0.01
        let a: Vec<f32> = (0..384).map(|i| ((i % 7) as f32 - 3.0).sin()).collect();
        let b: Vec<f32> = (0..384).map(|i| ((i % 5) as f32 - 2.0).cos()).collect();
        let qa = deserialize_q(&serialize_q(&a)).expect("dequant a");
        let qb = deserialize_q(&serialize_q(&b)).expect("dequant b");
        assert_eq!(serialize_q(&a).len(), 384 + 5, "v2 = dim + 5 байт");
        let dist = (cosine(&a, &qa) - 1.0).abs();
        assert!(dist < 0.01, "косинусное искажение self {dist}");
        let cross = (cosine(&a, &b) - cosine(&qa, &qb)).abs();
        assert!(cross < 0.01, "искажение перекрёстного косинуса {cross}");
    }

    #[test]
    fn test_dual_read() {
        let v = vec![0.5f32, -1.0, 2.0, 3.5];
        // легаси читается (без потерь)
        assert_eq!(deserialize(&serialize(&v)), Some(v.clone()));
        // v2 читается dual-read'ом (квантование lossy — сравнение с допуском)
        let restored = deserialize(&serialize_q(&v)).expect("v2 must deserialize");
        assert_eq!(v.len(), restored.len());
        let max_err = v
            .iter()
            .zip(restored.iter())
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(max_err < 0.03, "ошибка квантования {max_err}");
        // v2 не путается с легаси по размеру
        assert_eq!(serialize_q(&v).len(), 9);
        assert_eq!(serialize(&v).len(), 16);
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
