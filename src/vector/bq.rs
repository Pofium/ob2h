//! Двухступенчатый векторный поиск с mean-centered Binary Quantization (Ф72b.2):
//! 1) Бинарное квантование `val[i] > mean[i]` (или `val[i] > vec_mean` как fallback)
//!    в битовые маски `[u64; N]` (384 dim → 48 байт, 768 dim → 96 байт).
//!    Отбор top-oversample (400–800) по Hamming distance `popcnt(a ^ b)`.
//! 2) Точный FP32/INT8 rescore отобранных кандидатов до итогового top-k.

use rusqlite::{params, Connection};

use crate::vector::similarity::{cosine, deserialize, top_k};

/// Пространство имён для партиционированного векторного индекса (Ф72b.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorNamespace {
    /// Долговременная память (`memories`).
    Memories,
    /// Узлы кодового/знаниевого графа (`graph_nodes`).
    AstNodes,
}

impl VectorNamespace {
    /// Имя SQL-таблицы пространства имён.
    pub fn table(self) -> &'static str {
        match self {
            Self::Memories => "memories",
            Self::AstNodes => "graph_nodes",
        }
    }
}

/// Поиск по партиционированному пространству имён с фильтрацией по `project_id` (Ф72b.3).
pub fn search_namespace(
    conn: &Connection,
    ns: VectorNamespace,
    project_id: Option<&str>,
    q_vec: &[f32],
    limit: usize,
    min_score: f32,
) -> rusqlite::Result<Vec<(i64, f32)>> {
    search_table_two_stage(conn, ns.table(), project_id, q_vec, limit, min_score)
}

/// Дефолтный oversample для первой ступени BQ (400..800 кандидатов).
pub const DEFAULT_BQ_OVERSAMPLE: usize = 400;

/// Размер выборки для расчёта пер-дименсиональных средних `mu_i`.
const MEANS_SAMPLE_LIMIT: usize = 2048;

/// Минимум векторов для сохранения стабильных per-dimension средних.
const MIN_SAMPLES_FOR_MEANS: usize = 16;

/// Возвращает oversample для первой ступени BQ с учётом `OB2H_BQ_OVERSAMPLE` и `limit`.
pub fn oversample_for_limit(limit: usize) -> usize {
    let env_os = std::env::var("OB2H_BQ_OVERSAMPLE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BQ_OVERSAMPLE)
        .clamp(100, 2000);
    env_os.max(limit.saturating_mul(4)).min(2000).max(limit)
}

/// Вычисляет per-dimension средние `mu_i` по выборке векторов.
pub fn compute_per_dim_means(vectors: &[&[f32]]) -> Vec<f32> {
    let Some(first) = vectors.first() else {
        return Vec::new();
    };
    let dim = first.len();
    if dim == 0 {
        return Vec::new();
    }
    let mut sums = vec![0.0f64; dim];
    let mut count = 0usize;
    for v in vectors {
        if v.len() != dim {
            continue;
        }
        for (s, &x) in sums.iter_mut().zip(v.iter()) {
            *s += x as f64;
        }
        count += 1;
    }
    if count == 0 {
        return Vec::new();
    }
    let inv = 1.0 / (count as f64);
    sums.into_iter().map(|s| (s * inv) as f32).collect()
}

/// Загружает сохранённые per-dimension средние из `kv` или вычисляет их по выборке из таблицы.
pub fn load_or_compute_means(conn: &Connection, table: &str, dim: usize) -> Option<Vec<f32>> {
    if dim == 0 {
        return None;
    }
    let kv_key = format!("bq_means:{table}:{dim}");
    if let Ok(raw) = conn.query_row(
        "SELECT value FROM kv WHERE key = ?1",
        params![kv_key],
        |r| r.get::<_, String>(0),
    ) {
        if let Ok(parsed) = serde_json::from_str::<Vec<f32>>(&raw) {
            if parsed.len() == dim {
                return Some(parsed);
            }
        }
    }

    let sql = format!(
        "SELECT embedding FROM {table} \
         WHERE embedding IS NOT NULL AND deleted_at IS NULL \
         LIMIT {MEANS_SAMPLE_LIMIT}"
    );
    let mut stmt = conn.prepare(&sql).ok()?;
    let rows = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))
        .ok()?;
    let mut decoded: Vec<Vec<f32>> = Vec::new();
    for blob in rows.flatten() {
        if let Some(v) = deserialize(&blob) {
            if v.len() == dim {
                decoded.push(v);
            }
        }
    }
    if decoded.len() < MIN_SAMPLES_FOR_MEANS {
        return None;
    }
    let refs: Vec<&[f32]> = decoded.iter().map(|v| v.as_slice()).collect();
    let means = compute_per_dim_means(&refs);
    if means.len() == dim {
        if let Ok(json) = serde_json::to_string(&means) {
            let _ = conn.execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![kv_key, json],
            );
        }
        Some(means)
    } else {
        None
    }
}

/// Mean-centered Binary Quantization:
/// бит `i` = `vec[i] > means[i]` (или `vec[i] > vec_mean` при отсутствии `means`).
/// Упаковка в `u64`-слова (little-endian байты: 384 dim → 48 байт, 768 dim → 96 байт).
pub fn quantize_mean_centered(vec: &[f32], means: Option<&[f32]>) -> Vec<u8> {
    if vec.is_empty() {
        return Vec::new();
    }
    let word_count = vec.len().div_ceil(64);
    let mut words = vec![0u64; word_count];

    match means {
        Some(m) if m.len() == vec.len() => {
            for (i, (&val, &mu)) in vec.iter().zip(m.iter()).enumerate() {
                if val > mu {
                    words[i / 64] |= 1u64 << (i % 64);
                }
            }
        }
        _ => {
            let sum: f64 = vec.iter().map(|&x| x as f64).sum();
            let vec_mean = (sum / vec.len() as f64) as f32;
            for (i, &val) in vec.iter().enumerate() {
                if val > vec_mean {
                    words[i / 64] |= 1u64 << (i % 64);
                }
            }
        }
    }

    let mut out = Vec::with_capacity(word_count * 8);
    for w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Расстояние Хэмминга через `u64::count_ones` (компилируется в аппаратный `popcnt`).
#[inline]
pub fn hamming_distance(a: &[u8], b: &[u8]) -> u32 {
    let mut dist = 0u32;
    let mut chunks_a = a.chunks_exact(8);
    let mut chunks_b = b.chunks_exact(8);
    for (ca, cb) in (&mut chunks_a).zip(&mut chunks_b) {
        let wa = u64::from_le_bytes([ca[0], ca[1], ca[2], ca[3], ca[4], ca[5], ca[6], ca[7]]);
        let wb = u64::from_le_bytes([cb[0], cb[1], cb[2], cb[3], cb[4], cb[5], cb[6], cb[7]]);
        dist += (wa ^ wb).count_ones();
    }
    for (&ra, &rb) in chunks_a.remainder().iter().zip(chunks_b.remainder().iter()) {
        dist += (ra ^ rb).count_ones();
    }
    dist
}

/// Двухступенчатый поиск в таблице БД (`memories` или `graph_nodes`) с опциональной
/// фильтрацией по `project_id`:
/// 1) Отбор top-oversample по Hamming `popcnt` на `bq_mask`;
/// 2) Точный FP32/INT8 cosine rescore из `embedding` для отобранных кандидатов.
pub fn search_table_two_stage(
    conn: &Connection,
    table: &str,
    project_id: Option<&str>,
    q_vec: &[f32],
    limit: usize,
    min_score: f32,
) -> rusqlite::Result<Vec<(i64, f32)>> {
    let span = tracing::info_span!("vec", table = %table, limit);
    let _enter = span.enter();
    let started = std::time::Instant::now();
    if limit == 0 || q_vec.is_empty() {
        return Ok(Vec::new());
    }
    let means = load_or_compute_means(conn, table, q_vec.len());
    let q_mask = quantize_mean_centered(q_vec, means.as_deref());
    let mask_len = q_mask.len();
    let os = oversample_for_limit(limit);

    let where_project = if project_id.is_some() {
        " AND project_id = ?1"
    } else {
        ""
    };
    let stage1_sql = format!(
        "SELECT id, bq_mask FROM {table} \
         WHERE embedding IS NOT NULL AND deleted_at IS NULL{where_project}"
    );
    let mut stmt = conn.prepare(&stage1_sql)?;
    let mut masked: Vec<(i64, u32)> = Vec::new();
    let mut unmasked_ids: Vec<i64> = Vec::new();

    let mut collect_row = |id: i64, mask: Option<Vec<u8>>| {
        match mask {
            Some(m) if m.len() == mask_len => {
                let d = hamming_distance(&q_mask, &m);
                masked.push((id, d));
            }
            _ => unmasked_ids.push(id),
        }
    };

    if let Some(pid) = project_id {
        let rows = stmt.query_map(params![pid], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
        })?;
        for row in rows.flatten() {
            collect_row(row.0, row.1);
        }
    } else {
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
        })?;
        for row in rows.flatten() {
            collect_row(row.0, row.1);
        }
    }

    // Если масок нет вовсе — прямой перебор без лишних SQL-запросов
    if masked.is_empty() {
        if unmasked_ids.is_empty() {
            return Ok(Vec::new());
        }
        return brute_force_table(conn, table, project_id, q_vec, limit, min_score);
    }

    if masked.len() > os {
        masked.select_nth_unstable_by_key(os, |&(_, dist)| dist);
        masked.truncate(os);
    }

    let mut stage2_ids: Vec<i64> = masked.into_iter().map(|(id, _)| id).collect();
    stage2_ids.extend(unmasked_ids);

    // Ступень 2: точный rescore по полным (или INT8) векторам пакетами <= 900 параметров
    let mut scored: Vec<(i64, f32)> = Vec::with_capacity(stage2_ids.len());
    for chunk in stage2_ids.chunks(900) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT id, embedding FROM {table} WHERE id IN ({placeholders}) AND embedding IS NOT NULL"
        );
        let mut s2 = conn.prepare(&sql)?;
        let params_vec: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rows = s2.query_map(params_vec.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows.flatten() {
            if let Some(v) = deserialize(&row.1) {
                let sc = cosine(q_vec, &v);
                if sc >= min_score {
                    scored.push((row.0, sc));
                }
            }
        }
    }

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.truncate(limit);
    crate::graph::telemetry::stage_metrics::record_us("vec", started.elapsed().as_micros() as u64);
    Ok(scored)
}

fn brute_force_table(
    conn: &Connection,
    table: &str,
    project_id: Option<&str>,
    q_vec: &[f32],
    limit: usize,
    min_score: f32,
) -> rusqlite::Result<Vec<(i64, f32)>> {
    let where_project = if project_id.is_some() {
        " AND project_id = ?1"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, embedding FROM {table} \
         WHERE embedding IS NOT NULL AND deleted_at IS NULL{where_project}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut cands: Vec<(i64, Vec<u8>)> = Vec::new();
    if let Some(pid) = project_id {
        let rows = stmt.query_map(params![pid], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for r in rows.flatten() {
            cands.push(r);
        }
    } else {
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for r in rows.flatten() {
            cands.push(r);
        }
    }
    let refs: Vec<(i64, Option<&[u8]>)> = cands
        .iter()
        .map(|(id, b)| (*id, Some(b.as_slice())))
        .collect();
    Ok(top_k(q_vec, &refs, limit, min_score))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::vector::similarity::serialize_q;

    fn pseudo_e5_vectors(count: usize, dim: usize) -> Vec<Vec<f32>> {
        // Имитация анизотропии E5: смещение +0.08 по всем координатам + небольшие осцилляции
        (0..count)
            .map(|i| {
                let mut v = Vec::with_capacity(dim);
                for d in 0..dim {
                    let phase = ((i * 131 + d * 17) % 97) as f32 / 97.0 - 0.5;
                    let val = 0.08 + 0.04 * phase + if (i % 10) == (d % 10) { 0.03 } else { 0.0 };
                    v.push(val);
                }
                v
            })
            .collect()
    }

    #[test]
    fn test_bq_mask_sizes_384_and_768() {
        let v384 = vec![0.1f32; 384];
        let v768 = vec![0.1f32; 768];
        assert_eq!(quantize_mean_centered(&v384, None).len(), 48);
        assert_eq!(quantize_mean_centered(&v768, None).len(), 96);
    }

    #[test]
    fn test_bq_mean_centered_balances_anisotropic_e5_and_preserves_recall() {
        let dim = 384;
        let vecs = pseudo_e5_vectors(600, dim);
        let refs: Vec<&[f32]> = vecs.iter().map(|v| v.as_slice()).collect();
        let means = compute_per_dim_means(&refs);

        // Проверяем баланс бит 0/1: без центрирования (>0.0) почти все биты = 1;
        // с mean-centered BQ доля единиц около 0.50.
        let sample_mask = quantize_mean_centered(&vecs[0], Some(&means));
        let ones: u32 = sample_mask.iter().map(|b| b.count_ones()).sum();
        let ratio = ones as f32 / dim as f32;
        assert!(
            (0.30..=0.70).contains(&ratio),
            "ожидался баланс бит около 50/50, получено {ratio:.2}"
        );

        let db = Database::in_memory().expect("db");
        db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            for (i, v) in vecs.iter().enumerate() {
                let blob = serialize_q(v);
                let mask = quantize_mean_centered(v, Some(&means));
                tx.execute(
                    "INSERT INTO memories (key, content, category, embedding, bq_mask, created_at, updated_at) \
                     VALUES (?1, ?2, 'general', ?3, ?4, datetime('now'), datetime('now'))",
                    params![format!("k{i}"), format!("content {i}"), blob, mask],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .expect("seed");

        let q = &vecs[42];
        let approx = db
            .with_conn(|conn| search_table_two_stage(conn, "memories", None, q, 10, 0.0))
            .expect("bq search");
        let brute = db
            .with_conn(|conn| brute_force_table(conn, "memories", None, q, 10, 0.0))
            .expect("brute");

        let approx_ids: std::collections::HashSet<i64> = approx.iter().map(|x| x.0).collect();
        let hits = brute.iter().filter(|x| approx_ids.contains(&x.0)).count();
        let recall = hits as f64 / brute.len() as f64;
        assert!(
            recall >= 0.90,
            "BQ two-stage recall@10 на анизотропных векторах = {recall:.3}"
        );
    }

    #[test]
    fn test_hamming_distance_popcnt() {
        let a = vec![0b1010_1010u8; 48];
        let mut b = vec![0b1010_1010u8; 48];
        b[0] = 0b0010_1010; // 1 бит
        b[47] = 0b1010_1011; // 1 бит
        assert_eq!(hamming_distance(&a, &b), 2);
    }

    #[test]
    fn test_namespace_partitioning_isolates_memories_and_ast_nodes_by_project_id() {
        let db = Database::in_memory().expect("db");
        let v_a = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let v_b = vec![0.99f32, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mask_a = quantize_mean_centered(&v_a, None);
        let mask_b = quantize_mean_centered(&v_b, None);

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memories (key, content, category, embedding, bq_mask, project_id, created_at, updated_at) \
                 VALUES ('m_a', 'alpha', 'general', ?1, ?2, 'proj_a', datetime('now'), datetime('now'))",
                params![serialize_q(&v_a), mask_a],
            )?;
            conn.execute(
                "INSERT INTO memories (key, content, category, embedding, bq_mask, project_id, created_at, updated_at) \
                 VALUES ('m_b', 'beta', 'general', ?1, ?2, 'proj_b', datetime('now'), datetime('now'))",
                params![serialize_q(&v_b), mask_b],
            )?;
            conn.execute(
                "INSERT INTO graph_nodes (node_id, node_type, label, embedding, bq_mask, project_id, created_at, updated_at) \
                 VALUES ('fn:a', 'function', 'fn_a', ?1, ?2, 'proj_a', datetime('now'), datetime('now'))",
                params![serialize_q(&v_a), mask_a],
            )?;
            conn.execute(
                "INSERT INTO graph_nodes (node_id, node_type, label, embedding, bq_mask, project_id, created_at, updated_at) \
                 VALUES ('fn:b', 'function', 'fn_b', ?1, ?2, 'proj_b', datetime('now'), datetime('now'))",
                params![serialize_q(&v_b), mask_b],
            )?;
            Ok(())
        })
        .expect("seed");

        db.with_conn(|conn| {
            let mem_a = search_namespace(conn, VectorNamespace::Memories, Some("proj_a"), &v_a, 10, 0.0)?;
            assert_eq!(mem_a.len(), 1);
            let key: String = conn.query_row("SELECT key FROM memories WHERE id = ?1", params![mem_a[0].0], |r| r.get(0))?;
            assert_eq!(key, "m_a");

            let ast_b = search_namespace(conn, VectorNamespace::AstNodes, Some("proj_b"), &v_a, 10, 0.0)?;
            assert_eq!(ast_b.len(), 1);
            let nid: String = conn.query_row("SELECT node_id FROM graph_nodes WHERE id = ?1", params![ast_b[0].0], |r| r.get(0))?;
            assert_eq!(nid, "fn:b");
            Ok(())
        })
        .expect("search");
    }
}
