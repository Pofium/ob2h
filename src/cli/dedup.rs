//! Ф31.5 (PLAN_v1.4): `ob2h memory dedup [--dry-run]` — отчёт о почти-дублях
//! памяти (дешёвый косинус, без LLM): группы-кандидаты, предлагаемый
//! канонический ключ, план слияния.
//!
//! Авто-слияние запрещено (правило №1): без `--dry-run` пара лишь ПОМЕЧАЕТСЯ
//! (`meta.merge_candidate` на менее доверенной записи) — слияние решает дрим
//! (31.2) или явный `memory_merge` (31.4). `--dry-run` — только отчёт.

use std::collections::HashSet;

use rusqlite::params;

use crate::db::Database;
use crate::vector::similarity::{cosine, deserialize};

/// Порог identity-дубля (31.1: «одно и то же»).
pub const IDENTITY_COS: f32 = 0.98;
/// Нижняя граница подозрения на дубль (согласована с 31.1).
pub const SUSPECT_COS: f32 = 0.75;
/// Верхняя граница candidates-скана (живой БД хватает с запасом).
const SCAN_CAP: usize = 5000;

#[derive(Debug, Clone)]
pub struct GuessPair {
    pub a_key: String,
    pub b_key: String,
    pub cos: f32,
    /// true — identity-дубль (cos ≥ 0.98); false — подозрение 0.75–0.98.
    pub identity: bool,
    /// Предлагаемый канонический ключ (выше trust; при равенстве — выше importance).
    pub canonical_key: String,
}

struct Row {
    id: i64,
    key: String,
    content: String,
    trust: f64,
    importance: f64,
    vec: Option<Vec<f32>>,
}

/// Топ-1 косинусный сосед для каждой записи (без LLM, офлайн).
pub fn collect_pairs(db: &Database) -> anyhow::Result<Vec<GuessPair>> {
    let rows: Vec<Row> = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, key, content, trust, importance, embedding FROM memories \
             WHERE deleted_at IS NULL ORDER BY id LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![SCAN_CAP], |r| {
            Ok(Row {
                id: r.get(0)?,
                key: r.get(1)?,
                content: r.get::<_, String>(2)?.chars().take(80).collect(),
                trust: r.get(3)?,
                importance: r.get(4)?,
                vec: r.get::<_, Option<Vec<u8>>>(5)?.as_deref().and_then(deserialize),
            })
        })?;
        Ok(rows.flatten().collect())
    })?;

    // Лучший сосед для каждой записи, затем дедуп пар.
    #[derive(Clone)]
    struct Best {
        other: usize,
        cos: f32,
    }
    let mut best: Vec<Option<Best>> = vec![None; rows.len()];
    for i in 0..rows.len() {
        let (Some(vi), _) = (rows[i].vec.as_ref(), ()) else {
            continue;
        };
        for j in (i + 1)..rows.len() {
            let Some(vj) = rows[j].vec.as_ref() else {
                continue;
            };
            let c = cosine(vi, vj);
            if c < SUSPECT_COS {
                continue;
            }
            let better =
                |cur: &Option<Best>, c: f32| cur.as_ref().map(|b| c > b.cos).unwrap_or(true);
            if better(&best[i], c) {
                best[i] = Some(Best { other: j, cos: c });
            }
            if better(&best[j], c) {
                best[j] = Some(Best { other: i, cos: c });
            }
        }
    }

    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    let mut pairs = Vec::new();
    for (i, b) in best.iter().enumerate() {
        let Some(b) = b else { continue };
        let (a_idx, o_idx) = (i, b.other);
        let pair = if rows[a_idx].id < rows[o_idx].id {
            (rows[a_idx].id, rows[o_idx].id)
        } else {
            (rows[o_idx].id, rows[a_idx].id)
        };
        if !seen.insert(pair) {
            continue;
        }
        let (x, y) = (&rows[a_idx], &rows[o_idx]);
        let canonical = if x.trust != y.trust {
            if x.trust > y.trust { x } else { y }
        } else if x.importance >= y.importance {
            x
        } else {
            y
        };
        pairs.push(GuessPair {
            a_key: x.key.clone(),
            b_key: y.key.clone(),
            cos: b.cos,
            identity: b.cos >= IDENTITY_COS,
            canonical_key: canonical.key.clone(),
        });
    }
    pairs.sort_by(|p, q| q.cos.partial_cmp(&p.cos).unwrap_or(std::cmp::Ordering::Equal));
    Ok(pairs)
}

/// Помечаем пары маркером `meta.merge_candidate` на менее доверенной записи.
/// Возвращает число помеченных записей.
pub fn apply_markers(db: &Database, pairs: &[GuessPair]) -> anyhow::Result<usize> {
    let mut marked = 0;
    for p in pairs {
        // маркер на запись, которая НЕ каноническая (потенциально поглощаемая)
        let (target_key, other_key): (&str, &str) = if p.canonical_key == p.a_key {
            (&p.b_key, &p.a_key)
        } else {
            (&p.a_key, &p.b_key)
        };
        let updated = db.with_conn(|conn| {
            let meta: Option<String> = conn
                .query_row(
                    "SELECT meta FROM memories WHERE key = ?1 AND deleted_at IS NULL",
                    params![target_key],
                    |r| r.get(0),
                )
                .ok();
            let mut v: serde_json::Value =
                serde_json::from_str(&meta.unwrap_or_else(|| "{}".into())).unwrap_or(serde_json::json!({}));
            if v.get("merge_candidate").and_then(|m| m.as_str()) == Some(other_key) {
                return Ok(false); // уже помечена той же парой
            }
            if let Some(obj) = v.as_object_mut() {
                obj.insert("merge_candidate".to_string(), serde_json::json!(other_key));
            }
            conn.execute(
                "UPDATE memories SET meta = ?1 WHERE key = ?2 AND deleted_at IS NULL",
                params![v.to_string(), target_key],
            )?;
            Ok(true)
        })?;
        if updated {
            marked += 1;
        }
    }
    Ok(marked)
}

pub fn print_report(pairs: &[GuessPair], dry_run: bool) {
    if pairs.is_empty() {
        println!("0 candidates");
        return;
    }
    let identity = pairs.iter().filter(|p| p.identity).count();
    println!(
        "ob2h memory dedup — кандидатов: {} (identity-дублей: {}, подозрений: {})",
        pairs.len(),
        identity,
        pairs.len() - identity
    );
    println!();
    for (i, p) in pairs.iter().enumerate() {
        println!(
            "{:>3}. [{:.3}] {} ← {}{}",
            i + 1,
            p.cos,
            p.canonical_key,
            if p.canonical_key == p.a_key { &p.b_key } else { &p.a_key },
            if p.identity {
                "  (identity-дубль: при повторном save — тихий UPDATE, 31.1)"
            } else {
                "  (подозрение → дрим-вердикт или memory_merge)"
            },
        );
    }
    println!();
    if dry_run {
        println!("--dry-run: маркеры НЕ ставились, только отчёт.");
    } else {
        println!(
            "Маркеры merge_candidate поставлены — слияние выполнит дрим (LLM-вердикт, отчёт в dream_status) или явный memory_merge."
        );
    }
}

