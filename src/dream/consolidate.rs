//! Ф31 (PLAN_v1.4): офлайн-консолидация памяти в дриме.
//!
//! 31.2 — LLM-вердикты по группам `meta.merge_candidate` (маркеры ставит
//! дешёвый save-time пре-чек 31.1): `merge | keep_both | contradicts | supersedes`
//! (исходы MELD). На горячем пути save LLM нет. merge — тихий UPDATE канонической
//! записи + tombstone поглощённой (`meta.merged_into`) с редиректом memory_links;
//! supersedes/contradicts — обе записи живы + typed edge (резерв 23.5 начинает
//! работать). Авто-слияние разрешено только как дрим-вердикт с записью в отчёт
//! (31.4, правило №1).
//!
//! 31.3 — compaction (OMEGA-style): раз в 30 дней кластеризация записей с низким
//! trust/важностью и давним доступом (Jaccard по ключам + косинус) → LLM пишет
//! summary-узел `hmem-digest/<…>`, связанный kind=summary с членами. Кластер ≤ 8;
//! high-trust (≥ 0.7) и high-access не попадают. Оригиналы не трогаются —
//! дополнение к candidate_for_forget, не замена.

use std::collections::{HashMap, HashSet};

use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use super::Dream;
use crate::db::utcnow;
use crate::llm::LLMClientExt;
use crate::vector::similarity::{cosine, deserialize};

/// Максимум групп-кандидатов за один дрим (31.2).
pub const MAX_GROUPS_PER_DREAM: usize = 5;
/// Максимум записей в кластере compaction (31.3: больше — дайджест теряет конкретику).
pub const CLUSTER_MAX: usize = 8;
/// High-trust записи в кластеры не попадают (31.3).
pub const HIGH_TRUST_EXCLUDE: f64 = 0.7;
/// High-access записи в кластеры не попадают (насыщение access ~50 использованиям).
pub const HIGH_ACCESS_EXCLUDE: i64 = 20;
/// Компа́кция не чаще раза в 30 дней (31.3).
pub const COMPACTION_INTERVAL_DAYS: i64 = 30;

pub const CONSOLIDATE_SYSTEM: &str = "\
Ты — редактор памяти личного агента. Тебе дают две записи памяти. Верни СТРОГО JSON: \
{\"verdict\": \"merge|keep_both|contradicts|supersedes\", \"canonical_key\": \"ключ канонической записи (только для merge)\", \"note\": \"кратко почему\"}. \
merge — одно и то же знание, сформулированное дважды; supersedes — B обновляет состояние A \
(«раньше X, теперь Y» — обе записи остаются, история не теряется); contradicts — записи \
противоречат друг другу; keep_both — это разные знания.";

#[derive(Debug, Clone)]
struct Candidate {
    id: i64,
    key: String,
    content: String,
    trust: f64,
    importance: f64,
    meta: String,
    access_count: i64,
    updated_at: String,
}

#[derive(Deserialize)]
struct MergeVerdict {
    verdict: String,
    #[serde(default)]
    canonical_key: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

impl Dream {
    /// Ф31: офлайн-консолидация. Отчёт — в DreamStats.consolidation (dream_status).
    pub async fn consolidate_memory(&self) -> anyhow::Result<Value> {
        let mut report = json!({
            "merges": [],
            "edges": [],
            "digests": [],
            "keep_both": [],
            "groups_seen": 0,
        });
        if let Err(e) = self.consolidate_merge_candidates(&mut report).await {
            warn!("Консолидация merge-кандидатов не удалась: {e}");
        }
        if let Err(e) = self.compaction(&mut report).await {
            warn!("Compaction не удалась: {e}");
        }
        Ok(report)
    }

    // --- 31.2: LLM-вердикты по merge-кандидатам ----------------------------

    async fn consolidate_merge_candidates(&self, report: &mut Value) -> anyhow::Result<()> {
        let marked = self.load_marked_candidates()?;
        if marked.is_empty() {
            return Ok(());
        }

        // Пары (запись с маркером → сосед), дедуп по неупорядоченной паре id.
        let mut seen: HashSet<(i64, i64)> = HashSet::new();
        let mut pairs: Vec<(Candidate, Candidate)> = Vec::new();
        for cand in &marked {
            let Some(target_key) = marker_target(&cand.meta) else {
                continue;
            };
            let Some(neighbor) = self.load_candidate(&target_key)? else {
                // сосед исчез (tombstone/forget) — маркер больше не нужен
                self.clear_marker(cand)?;
                continue;
            };
            let pair_key = if cand.id < neighbor.id {
                (cand.id, neighbor.id)
            } else {
                (neighbor.id, cand.id)
            };
            if !seen.insert(pair_key) {
                continue;
            }
            if pairs.len() >= MAX_GROUPS_PER_DREAM {
                break;
            }
            pairs.push((cand.clone(), neighbor));
        }
        report["groups_seen"] = json!(pairs.len());
        if pairs.is_empty() {
            return Ok(());
        }

        for (a, b) in pairs {
            let prompt = format!(
                "Консолидация памяти — две записи:\n\nA: key={} (trust {:.2}):\n{}\n\nB: key={} (trust {:.2}):\n{}\n\nТвой вердикт (строго JSON):",
                a.key, a.trust, a.content, b.key, b.trust, b.content
            );
            let verdict: MergeVerdict = match self.llm.ask_json(&prompt, Some(CONSOLIDATE_SYSTEM)).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("LLM-вердикт для {}/{} не удался: {e}", a.key, b.key);
                    continue; // маркеры сохраняются — попробуем в следующем дриме
                }
            };
            match verdict.verdict.as_str() {
                "merge" => {
                    let canonical = if Some(&a.key) == verdict.canonical_key.as_ref() {
                        &a
                    } else if Some(&b.key) == verdict.canonical_key.as_ref() {
                        &b
                    } else if a.trust >= b.trust {
                        &a
                    } else {
                        &b
                    };
                    let absorbed = if canonical.id == a.id { &b } else { &a };
                    self.apply_merge(canonical, absorbed)?;
                    report["merges"].as_array_mut().unwrap().push(json!({
                        "canonical": canonical.key,
                        "absorbed": absorbed.key,
                        "note": verdict.note,
                    }));
                }
                "supersedes" | "contradicts" => {
                    // направленная связь: более свежая запись → более старая
                    let (from, to) = if a.updated_at >= b.updated_at { (&a, &b) } else { (&b, &a) };
                    self.insert_edge(from.id, to.id, &verdict.verdict)?;
                    self.clear_marker(&a)?;
                    self.clear_marker(&b)?;
                    report["edges"].as_array_mut().unwrap().push(json!({
                        "from": from.key,
                        "to": to.key,
                        "kind": verdict.verdict,
                        "note": verdict.note,
                    }));
                }
                _ => {
                    // keep_both и нераспознанный вердикт: ничего не меняется,
                    // маркер снимается (группа уже отработана)
                    self.clear_marker(&a)?;
                    self.clear_marker(&b)?;
                    report["keep_both"].as_array_mut().unwrap().push(json!({
                        "keys": [a.key, b.key],
                        "note": verdict.note,
                    }));
                }
            }
        }
        Ok(())
    }

    fn load_marked_candidates(&self) -> anyhow::Result<Vec<Candidate>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, key, content, trust, importance, meta, access_count, updated_at \
                 FROM memories WHERE deleted_at IS NULL AND meta LIKE '%merge_candidate%' \
                 ORDER BY id LIMIT 40",
            )?;
            let rows = stmt.query_map([], row_candidate)?;
            Ok(rows.flatten().collect())
        })
    }

    fn load_candidate(&self, key: &str) -> anyhow::Result<Option<Candidate>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, key, content, trust, importance, meta, access_count, updated_at \
                 FROM memories WHERE deleted_at IS NULL AND key = ?1 LIMIT 1",
            )?;
            let mut rows = stmt.query(params![key])?;
            Ok(rows.next()?.map(row_candidate).transpose()?)
        })
    }

    /// Слияние: каноническая запись получает union meta, max importance, sum access;
    /// поглощённая — tombstone + meta.merged_into; links редиректятся на каноническую.
    fn apply_merge(&self, canonical: &Candidate, absorbed: &Candidate) -> anyhow::Result<()> {
        let merged_meta = union_meta(&canonical.meta, &absorbed.meta);
        let now = utcnow();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET importance = MAX(importance, ?1), access_count = access_count + ?2, \
                 meta = ?3, updated_at = ?4 WHERE id = ?5",
                params![absorbed.importance, absorbed.access_count, merged_meta, now, canonical.id],
            )?;
            // поглощённая: tombstone + merged_into (реплицируется синком как tombstone)
            let mut absorbed_meta: Value = serde_json::from_str(&absorbed.meta).unwrap_or(json!({}));
            if let Some(obj) = absorbed_meta.as_object_mut() {
                obj.remove("merge_candidate");
                obj.insert("merged_into".to_string(), json!(canonical.key));
            }
            conn.execute(
                "UPDATE memories SET deleted_at = ?1, updated_at = ?1, meta = ?2, origin = '' WHERE id = ?3",
                params![now, absorbed_meta.to_string(), absorbed.id],
            )?;
            Ok(())
        })?;
        self.redirect_links(absorbed.id, canonical.id)
    }

    /// Соседи поглощённой записи перепривязываются к канонической (без дублей PK).
    fn redirect_links(&self, absorbed_id: i64, canonical_id: i64) -> anyhow::Result<()> {
        self.db.with_conn(|conn| {
            // чтение — в отдельном scope: stmt держит borrow conn
            let links: Vec<(i64, i64, String, f64, String)> = {
                let mut stmt = conn.prepare(
                    "SELECT from_id, to_id, kind, weight, created_at FROM memory_links \
                     WHERE from_id = ?1 OR to_id = ?1",
                )?;
                let rows = stmt.query_map(params![absorbed_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, f64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?;
                rows.flatten().collect()
            };
            for (from, to, kind, weight, created) in links {
                let new_from = if from == absorbed_id { canonical_id } else { from };
                let new_to = if to == absorbed_id { canonical_id } else { to };
                if new_from == new_to {
                    continue; // самолинк не нужен
                }
                conn.execute(
                    "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, weight, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![new_from, new_to, kind, weight, created],
                )?;
            }
            conn.execute(
                "DELETE FROM memory_links WHERE from_id = ?1 OR to_id = ?1",
                params![absorbed_id],
            )?;
            Ok(())
        })
    }

    /// Typed edge (резерв 23.5 начинает работать); повторный дрим не дублирует.
    fn insert_edge(&self, from_id: i64, to_id: i64, kind: &str) -> anyhow::Result<()> {
        if from_id == to_id {
            return Ok(());
        }
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, weight, created_at) \
                 VALUES (?1, ?2, ?3, 1.0, ?4)",
                params![from_id, to_id, kind, utcnow()],
            )?;
            Ok(())
        })
    }

    /// Снять маркер merge_candidate (группа отработана).
    fn clear_marker(&self, cand: &Candidate) -> anyhow::Result<()> {
        let mut meta: Value = serde_json::from_str(&cand.meta).unwrap_or(json!({}));
        if let Some(obj) = meta.as_object_mut() {
            if obj.remove("merge_candidate").is_none() {
                return Ok(()); // маркера уже нет
            }
        }
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET meta = ?1 WHERE id = ?2",
                params![meta.to_string(), cand.id],
            )?;
            Ok(())
        })
    }

    // --- 31.3: compaction → summary-узлы ------------------------------------

    async fn compaction(&self, report: &mut Value) -> anyhow::Result<()> {
        // троттлинг: раз в 30 дней
        if let Some(last) = self.db.get_kv("compaction:last")? {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&last) {
                let age = chrono::Utc::now().signed_duration_since(dt.with_timezone(&chrono::Utc));
                if age.num_days() < COMPACTION_INTERVAL_DAYS {
                    return Ok(());
                }
            }
        }

        let Some(ref memory) = self.memory else {
            return Ok(());
        };

        // Кандидаты: низкий trust + низкая важность + давний доступ; high-trust и
        // high-access исключены (31.3); дайджесты самих себя не кластеризуем.
        let mut pool: Vec<Candidate> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, key, content, trust, importance, meta, access_count, updated_at \
                 FROM memories WHERE deleted_at IS NULL AND trust < 0.35 AND importance < 0.35 \
                 AND key NOT LIKE 'hmem-digest/%' ORDER BY trust ASC, updated_at ASC LIMIT 60",
            )?;
            let rows = stmt.query_map([], row_candidate)?;
            Ok(rows.flatten().collect())
        })?;
        // давний доступ — фильтр в Rust (форматы timestamp в SQL не сравнить честно)
        pool.retain(|c| c.access_count < HIGH_ACCESS_EXCLUDE);

        let clusters = self.cluster_candidates(&pool);
        for members in clusters.iter() {
            if members.len() < 2 || members.len() > CLUSTER_MAX {
                continue;
            }
            let list = members
                .iter()
                .map(|c| format!("- key={} (trust {:.2}): {}", c.key, c.trust, c.content))
                .collect::<Vec<_>>()
                .join("\n");
            let prompt = format!(
                "Сжатие памяти — кластер из {} слабых записей:\n{list}\n\nНапиши ОДИН компактный дайджест (2-4 предложения), сохраняющий конкретику (имена, числа, пути). Без преамбулы:",
                members.len()
            );
            let digest_text = match self.llm.ask(&prompt, Some(CONSOLIDATE_SYSTEM)).await {
                Ok(t) => t.trim().to_string(),
                Err(e) => {
                    warn!("Digest-вызов не удался: {e}");
                    continue;
                }
            };
            if digest_text.is_empty() {
                continue;
            }
            let key = format!("hmem-digest/{}", chrono::Utc::now().format("%Y-%m-%d"));
            let seq = self.db.with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT COUNT(*) FROM memories WHERE key LIKE ?1 || '%'",
                        params![format!("{key}%")],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0))
            })?;
            let key = if seq == 0 { key } else { format!("{key}-{}", seq + 1) };
            // оригиналы не трогаются: дайджест — дополнение к candidate_for_forget
            let digest_key = memory
                .save(&digest_text, Some(&key), "digest", 0.3, "dream", None)
                .await?;
            let digest_id: i64 = self.db.with_conn(|conn| {
                Ok(conn.query_row("SELECT id FROM memories WHERE key = ?1", params![digest_key], |r| {
                    r.get(0)
                })?)
            })?;
            for m in members {
                self.insert_edge(digest_id, m.id, "summary")?;
            }
            report["digests"].as_array_mut().unwrap().push(json!(digest_key));
        }

        self.db.set_kv(
            "compaction:last",
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )?;
        Ok(())
    }

    /// Жадная кластеризация: Jaccard по токенам ключа ИЛИ косинус эмбеддингов.
    fn cluster_candidates(&self, pool: &[Candidate]) -> Vec<Vec<Candidate>> {
        let vectors: HashMap<i64, Option<Vec<f32>>> = self
            .db
            .with_conn(|conn| {
                let mut map = HashMap::new();
                let mut stmt = conn.prepare("SELECT embedding FROM memories WHERE id = ?1")?;
                for c in pool {
                    let blob: Option<Vec<u8>> =
                        stmt.query_row(params![c.id], |r| r.get(0)).unwrap_or(None);
                    map.insert(c.id, blob.as_deref().and_then(deserialize));
                }
                Ok(map)
            })
            .unwrap_or_default();

        let mut used: HashSet<i64> = HashSet::new();
        let mut clusters: Vec<Vec<Candidate>> = Vec::new();
        for seed in pool {
            if used.contains(&seed.id) {
                continue;
            }
            let mut cluster = vec![seed.clone()];
            used.insert(seed.id);
            for cand in pool {
                if used.contains(&cand.id) {
                    continue;
                }
                if cluster.len() >= CLUSTER_MAX {
                    break;
                }
                let joined = cluster
                    .iter()
                    .any(|m| similar(m, cand, &vectors));
                if joined {
                    cluster.push(cand.clone());
                    used.insert(cand.id);
                }
            }
            clusters.push(cluster);
        }
        clusters
    }
}

// Ф31.2/31.3 реализованы как inherent impl Dream в этом модуле (тот же крейт).

/// Маркер 31.1: ключ соседа из meta.merge_candidate.
fn marker_target(meta: &str) -> Option<String> {
    let v: Value = serde_json::from_str(meta).ok()?;
    v.get("merge_candidate")?.as_str().map(str::to_string)
}

/// Union meta-JSON: ключи канонической записи выигрывают при коллизии.
fn union_meta(canonical: &str, absorbed: &str) -> String {
    let mut base: Value = serde_json::from_str(canonical).unwrap_or(json!({}));
    let extra: Value = serde_json::from_str(absorbed).unwrap_or(json!({}));
    if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            base_obj.entry(k.clone()).or_insert(v.clone());
        }
        // маркер в объединённой meta больше не нужен
        base_obj.remove("merge_candidate");
    }
    base.to_string()
}

/// Похожесть для кластеризации: Jaccard токенов ключа ≥ 0.5 или косинус ≥ 0.75.
fn similar(a: &Candidate, b: &Candidate, vectors: &HashMap<i64, Option<Vec<f32>>>) -> bool {
    let jac = jaccard(&a.key, &b.key);
    if jac >= 0.5 {
        return true;
    }
    if let (Some(Some(va)), Some(Some(vb))) = (vectors.get(&a.id), vectors.get(&b.id)) {
        return cosine(va, vb) >= 0.75;
    }
    false
}

fn tokens(s: &str) -> HashSet<&str> {
    s.split(|c: char| c == '-' || c == '/' || c == '_').collect()
}

fn jaccard(a: &str, b: &str) -> f64 {
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() && tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

fn row_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        id: row.get(0)?,
        key: row.get(1)?,
        content: row.get(2)?,
        trust: row.get(3)?,
        importance: row.get(4)?,
        // meta nullable (save() без meta пишет NULL) — нормализуем к "{}"
        meta: row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "{}".to_string()),
        access_count: row.get(6)?,
        updated_at: row.get(7)?,
    })
}
