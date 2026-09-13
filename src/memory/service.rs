//! Реализация MemoryService.

use std::collections::HashSet;
use std::sync::Arc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::{models::MemoryRecord, utcnow, Database};
use crate::embedding::EmbeddingProvider;
use crate::vector::{rrf_merge, serialize, top_k};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub record: MemoryRecord,
    pub score: f64,
    pub match_type: String, // "fts" | "vector" | "hybrid"
}

pub struct MemoryService {
    db: Database,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl MemoryService {
    pub fn new(db: Database, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { db, embedder }
    }

    /// Генерация детерминированного ключа по содержанию, если не передан.
    pub fn generate_key(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.trim().as_bytes());
        let hash = hex::encode(hasher.finalize());
        format!("m_{}", &hash[..12])
    }

    /// Сохранить воспоминание (upsert по ключу).
    pub async fn save(
        &self,
        content: &str,
        key: Option<&str>,
        category: &str,
        importance: f64,
        source: &str,
        meta: Option<&str>,
    ) -> anyhow::Result<String> {
        self.save_with_project(content, key, category, importance, source, meta, None).await
    }

    /// Сохранить воспоминание с опциональной привязкой к проекту.
    pub async fn save_with_project(
        &self,
        content: &str,
        key: Option<&str>,
        category: &str,
        importance: f64,
        source: &str,
        meta: Option<&str>,
        project_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let content = content.trim();
        if content.is_empty() {
            anyhow::bail!("content cannot be empty");
        }

        let k = key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| Self::generate_key(content));

        // Получаем векторное представление
        let embeddings = self.embedder.embed(&[content.to_string()]).await?;
        let emb_blob = embeddings.first().map(|v| serialize(v));

        let now = utcnow();

        self.db.with_conn(|conn| {
            conn.execute(
                r#"
                INSERT INTO memories (
                    key, content, category, importance, source, meta, embedding, created_at, updated_at, project_id
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)
                ON CONFLICT(key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    importance = excluded.importance,
                    source = excluded.source,
                    meta = excluded.meta,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at,
                    project_id = COALESCE(excluded.project_id, memories.project_id),
                    origin = '',           /* локальная правка — строка снова «наша» */
                    deleted_at = NULL      /* повторное сохранение снимает tombstone */
                "#,
                params![k, content, category, importance, source, meta, emb_blob, now, project_id],
            )?;
            Ok(())
        })?;

        // Ф23.5: детерминированные автосвязи (same_project/category) после сохранения.
        let row_id: i64 = self.db.with_conn(|conn| {
            conn.query_row("SELECT id FROM memories WHERE key = ?1", params![k], |r| r.get(0))
        })?;
        self.link_after_save(row_id, category, project_id)?;

        Ok(k)
    }

    /// Получить воспоминание по ключу.
    pub fn get(&self, key: &str) -> anyhow::Result<Option<MemoryRecord>> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, key, content, category, importance, source, meta, embedding, created_at, updated_at, access_count, last_accessed, project_id FROM memories WHERE key = ?1 AND deleted_at IS NULL",
                params![key],
                |row| {
                    Ok(MemoryRecord {
                        id: row.get(0)?,
                        key: row.get(1)?,
                        content: row.get(2)?,
                        category: row.get(3)?,
                        importance: row.get(4)?,
                        source: row.get(5)?,
                        meta: row.get(6)?,
                        embedding: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                        access_count: row.get(10)?,
                        last_accessed: row.get(11)?,
                        project_id: row.get(12)?,
                    })
                },
            )
            .optional()
        })
    }

    /// Обновить существующее воспоминание.
    pub async fn update(
        &self,
        key: &str,
        content: Option<&str>,
        importance: Option<f64>,
        category: Option<&str>,
    ) -> anyhow::Result<bool> {
        let existing = match self.get(key)? {
            Some(r) => r,
            None => return Ok(false),
        };

        let new_content = content.unwrap_or(&existing.content);
        let new_importance = importance.unwrap_or(existing.importance);
        let new_category = category.unwrap_or(&existing.category);

        let emb_blob = if content.is_some() {
            let embs = self.embedder.embed(&[new_content.to_string()]).await?;
            embs.first().map(|v| serialize(v))
        } else {
            existing.embedding
        };

        let now = utcnow();
        self.db.with_conn(|conn| {
            conn.execute(
                r#"
                UPDATE memories
                SET content = ?1, importance = ?2, category = ?3, embedding = ?4,
                    updated_at = ?5, origin = ''
                WHERE key = ?6
                "#,
                params![new_content, new_importance, new_category, emb_blob, now, key],
            )?;
            Ok(())
        })?;

        Ok(true)
    }

    /// Удалить воспоминание по ключу (tombstone — удаление реплицируется синком,
    /// физическая чистка отложена в maintenance автодрима).
    pub fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let now = utcnow();
        self.db.with_conn(|conn| {
            let count = conn.execute(
                "UPDATE memories SET deleted_at = ?1, updated_at = ?1, origin = ''
                 WHERE key = ?2 AND deleted_at IS NULL",
                params![now, key],
            )?;
            if count > 0 {
                // Ф23.5: каскад — tombstone записи рвут автосвязи (физически,
                // связи не синхронизируются и восстановимы пересохранением).
                conn.execute(
                    "DELETE FROM memory_links WHERE from_id IN (SELECT id FROM memories WHERE key = ?1) \
                     OR to_id IN (SELECT id FROM memories WHERE key = ?1)",
                    params![key],
                )?;
                return Ok(true);
            }
            // уже в tombstone или отсутствует: ключ мог быть удалён ранее
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE key = ?1",
                    params![key],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            Ok(exists)
        })
    }

    /// Полнотекстовый поиск FTS5 trigram.
    pub fn search_fts(&self, query: &str, limit: usize) -> anyhow::Result<Vec<(i64, f64)>> {
        let clean_query = query.trim();
        if clean_query.is_empty() {
            return Ok(Vec::new());
        }

        self.db.with_conn(|conn| {
            // Пытаемся выполнить FTS5 MATCH
            let mut stmt = conn.prepare(
                r#"
                SELECT rowid, rank
                FROM memories_fts
                WHERE memories_fts MATCH ?1
                ORDER BY rank
                LIMIT ?2
                "#,
            );

            let mut results = Vec::new();
            match stmt {
                Ok(ref mut s) => {
                    let rows = s.query_map(params![clean_query, limit as i64], |row| {
                        let id: i64 = row.get(0)?;
                        let rank: f64 = row.get(1)?;
                        Ok((id, -rank)) // FTS5 bm25 выдаёт отрицательные числа (меньше = лучше)
                    });
                    if let Ok(mapped) = rows {
                        for r in mapped.flatten() {
                            results.push(r);
                        }
                    }
                }
                Err(_) => {
                    // Фолбэк на LIKE
                    let like_pattern = format!("%{clean_query}%");
                    let mut s = conn.prepare(
                        "SELECT id, importance FROM memories WHERE content LIKE ?1 AND deleted_at IS NULL LIMIT ?2",
                    )?;
                    let rows = s.query_map(params![like_pattern, limit as i64], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?;
                    for r in rows.flatten() {
                        results.push(r);
                    }
                }
            }
            Ok(results)
        })
    }

    /// Векторный семантический поиск.
    pub async fn search_vector(
        &self,
        query: &str,
        limit: usize,
        min_score: f32,
    ) -> anyhow::Result<Vec<(i64, f64)>> {
        let q_embs = self.embedder.embed(&[query.to_string()]).await?;
        let q_vec = match q_embs.first() {
            Some(v) => v,
            None => return Ok(Vec::new()),
        };

        let candidates = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id, embedding FROM memories WHERE embedding IS NOT NULL AND deleted_at IS NULL")?;
            let rows = stmt.query_map([], |row| {
                let id: i64 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            })?;
            let mut list = Vec::new();
            for r in rows.flatten() {
                list.push(r);
            }
            Ok(list)
        })?;

        let candidate_refs: Vec<(i64, Option<&[u8]>)> = candidates
            .iter()
            .map(|(id, blob)| (*id, Some(blob.as_slice())))
            .collect();

        let scored = top_k(q_vec, &candidate_refs, limit, min_score);
        Ok(scored.into_iter().map(|(id, s)| (id, s as f64)).collect())
    }

    /// Гибридный поиск: FTS5 + Vector слияние через RRF (k=60). Трогает access-счётчики.
    pub async fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        min_score: f32,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        self.search_hybrid_hits(query, limit, min_score, true).await
    }

    /// Ядро гибридного поиска. `touch=false` — для пулов кандидатов (build_context
    /// трогает только записи, вошедшие в итоговый блок).
    pub async fn search_hybrid_hits(
        &self,
        query: &str,
        limit: usize,
        min_score: f32,
        touch: bool,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        let fts_res = self.search_fts(query, limit * 2).unwrap_or_default();
        let vec_res = self.search_vector(query, limit * 2, min_score).await.unwrap_or_default();

        let fts_ids: Vec<i64> = fts_res.iter().map(|(id, _)| *id).collect();
        let vec_ids: Vec<i64> = vec_res.iter().map(|(id, _)| *id).collect();

        let merged = rrf_merge(&fts_ids, &vec_ids, 60.0);
        let target_items = if merged.len() > limit {
            &merged[..limit]
        } else {
            &merged
        };

        if target_items.is_empty() {
            return Ok(Vec::new());
        }

        let mut hits = Vec::new();
        for item in target_items {
            if let Some(record) = self.get_by_id(item.id)? {
                let match_type = match (item.fts_rank, item.vector_rank) {
                    (Some(_), Some(_)) => "hybrid",
                    (Some(_), None) => "fts",
                    (None, Some(_)) => "vector",
                    _ => "unknown",
                };
                hits.push(MemoryHit {
                    record,
                    score: item.rrf_score,
                    match_type: match_type.to_string(),
                });
            }
        }

        // Обновляем счетчик обращений (только для явного поиска, не для пулов контекста)
        if touch {
            let ids: Vec<i64> = hits.iter().map(|h| h.record.id).collect();
            self.touch_access(&ids)?;
        }

        Ok(hits)
    }

    pub fn get_by_id(&self, id: i64) -> anyhow::Result<Option<MemoryRecord>> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, key, content, category, importance, source, meta, embedding, created_at, updated_at, access_count, last_accessed, project_id FROM memories WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| {
                    Ok(MemoryRecord {
                        id: row.get(0)?,
                        key: row.get(1)?,
                        content: row.get(2)?,
                        category: row.get(3)?,
                        importance: row.get(4)?,
                        source: row.get(5)?,
                        meta: row.get(6)?,
                        embedding: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                        access_count: row.get(10)?,
                        last_accessed: row.get(11)?,
                        project_id: row.get(12)?,
                    })
                },
            )
            .optional()
        })
    }

    fn touch_access(&self, ids: &[i64]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = utcnow();
        self.db.with_conn(|conn| {
            for id in ids {
                // Ф23.1: использование подтверждает запись — trust +0.02 (кламп 1.0).
                conn.execute(
                    "UPDATE memories SET access_count = access_count + 1, last_accessed = ?1, \
                     trust = MIN(1.0, trust + 0.02), last_feedback_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
            Ok(())
        })
    }

    /// Ф23.4: feedback агента. helpful +0.15 | unhelpful −0.2 | outdated −0.3.
    /// Пишет в meta.feedback (последние 20), возвращает новый trust (None — нет ключа).
    pub fn record_feedback(
        &self,
        key: &str,
        verdict: &str,
        note: Option<&str>,
    ) -> anyhow::Result<Option<f64>> {
        let delta = match verdict {
            "helpful" => 0.15,
            "unhelpful" => -0.2,
            "outdated" => -0.3,
            other => anyhow::bail!(
                "verdict должен быть helpful|unhelpful|outdated, получено: {other}"
            ),
        };
        let id = match self.get(key)? {
            Some(r) => r.id,
            None => return Ok(None),
        };

        let entry = serde_json::json!({ "verdict": verdict, "note": note, "at": utcnow() });
        self.db.with_conn(|conn| {
            // meta колонка nullable — читаем сразу в Option<String>
            let meta: Option<String> = conn.query_row(
                "SELECT meta FROM memories WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )?;
            let mut obj: serde_json::Map<String, serde_json::Value> = meta
                .as_deref()
                .and_then(|m| serde_json::from_str(m).ok())
                .unwrap_or_default();
            let mut feedback: Vec<serde_json::Value> = obj
                .get("feedback")
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            feedback.push(entry);
            let start = feedback.len().saturating_sub(20);
            obj.insert(
                "feedback".to_string(),
                serde_json::Value::from(feedback[start..].to_vec()),
            );
            let meta_str = serde_json::to_string(&obj)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            conn.execute(
                "UPDATE memories SET meta = ?1 WHERE id = ?2",
                params![meta_str, id],
            )?;
            Ok(())
        })?;

        let trust = self.apply_trust(id, delta)?;
        self.mark_forget_candidates()?;
        Ok(trust)
    }

    /// Ф23.2: вердикт dream-ревизии. confirmed +0.1 | outdated −0.4 | contradicted −0.5.
    pub fn revise_trust_by_key(&self, key: &str, verdict: &str) -> anyhow::Result<Option<f64>> {
        let delta = match verdict {
            "confirmed" => 0.1,
            "outdated" => -0.4,
            "contradicted" => -0.5,
            other => anyhow::bail!(
                "verdict должен быть confirmed|outdated|contradicted, получено: {other}"
            ),
        };
        let id = match self.get(key)? {
            Some(r) => r.id,
            None => return Ok(None),
        };
        let trust = self.apply_trust(id, delta)?;
        self.mark_forget_candidates()?;
        Ok(trust)
    }

    /// Кандидаты на dream-ревизию: минимальный trust, затем самые давние по feedback.
    pub fn lowest_trust(&self, n: usize) -> anyhow::Result<Vec<(MemoryRecord, f64)>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, key, content, category, importance, source, meta, embedding, \
                        created_at, updated_at, access_count, last_accessed, project_id, trust \
                 FROM memories WHERE deleted_at IS NULL \
                 ORDER BY trust ASC, (last_feedback_at IS NULL) DESC, last_feedback_at ASC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![n as i64], |row| {
                let rec = MemoryRecord {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: row.get(3)?,
                    importance: row.get(4)?,
                    source: row.get(5)?,
                    meta: row.get(6)?,
                    embedding: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    access_count: row.get(10)?,
                    last_accessed: row.get(11)?,
                    project_id: row.get(12)?,
                };
                Ok((rec, row.get::<_, f64>(13)?))
            })?;
            Ok(rows.flatten().collect())
        })
    }

    /// Сдвиг trust с клампом [0,1]; last_feedback_at = now. Возвращает новый trust.
    fn apply_trust(&self, id: i64, delta: f64) -> anyhow::Result<Option<f64>> {
        let now = utcnow();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET trust = MAX(0.0, MIN(1.0, trust + ?1)), last_feedback_at = ?2 \
                 WHERE id = ?3",
                params![delta, now, id],
            )?;
            let t: Option<f64> = conn
                .query_row("SELECT trust FROM memories WHERE id = ?1", params![id], |r| r.get(0))
                .optional()?;
            Ok(t)
        })
    }

    /// Ф23.3: trust < 0.15 → meta.candidate_for_forget=1. Никаких автоудалений —
    /// только явный memory_forget.
    fn mark_forget_candidates(&self) -> anyhow::Result<usize> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, meta FROM memories WHERE trust < 0.15 AND deleted_at IS NULL",
            )?;
            let rows: Vec<(i64, Option<String>)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .flatten()
                .collect();
            let mut marked = 0;
            for (id, meta) in rows {
                let mut obj: serde_json::Map<String, serde_json::Value> = meta
                    .as_deref()
                    .and_then(|m| serde_json::from_str(m).ok())
                    .unwrap_or_default();
                if obj.get("candidate_for_forget").and_then(|v| v.as_i64()) == Some(1) {
                    continue;
                }
                obj.insert("candidate_for_forget".to_string(), serde_json::Value::from(1));
                let meta_str = serde_json::to_string(&obj)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                conn.execute(
                    "UPDATE memories SET meta = ?1 WHERE id = ?2",
                    params![meta_str, id],
                )?;
                marked += 1;
            }
            Ok(marked)
        })
    }

    /// Ф23.5: автосвязи при save — same_project + category (до 5 самых свежих).
    /// Дедуп по PK (from_id, to_id, kind); kind=entity — резерв под экстрактор.
    fn link_after_save(&self, id: i64, category: &str, project_id: Option<&str>) -> anyhow::Result<()> {
        let now = utcnow();
        self.db.with_conn(|conn| {
            if let Some(pid) = project_id {
                conn.execute(
                    "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, created_at)
                     SELECT ?1, id, 'same_project', ?2 FROM memories
                     WHERE project_id = ?3 AND id != ?1 AND deleted_at IS NULL
                     ORDER BY updated_at DESC LIMIT 5",
                    params![id, now, pid],
                )?;
            }
            conn.execute(
                "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, created_at)
                 SELECT ?1, id, 'category', ?2 FROM memories
                 WHERE category = ?3 AND id != ?1 AND deleted_at IS NULL
                 ORDER BY updated_at DESC LIMIT 5",
                params![id, now, category],
            )?;
            Ok(())
        })
    }

    /// 1-hop соседи по memory_links (оба направления), без tombstone, до limit.
    pub fn related_records(&self, ids: &[i64], limit: usize) -> anyhow::Result<Vec<MemoryRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.db.with_conn(|conn| {
            let mut out: Vec<MemoryRecord> = Vec::new();
            let mut seen: HashSet<i64> = ids.iter().copied().collect();
            for &id in ids {
                let mut stmt = conn.prepare(
                    "SELECT m.id, m.key, m.content, m.category, m.importance, m.source, m.meta, \
                            m.embedding, m.created_at, m.updated_at, m.access_count, m.last_accessed, m.project_id \
                     FROM memory_links l JOIN memories m ON m.id = l.to_id \
                     WHERE l.from_id = ?1 AND m.deleted_at IS NULL \
                     UNION ALL \
                     SELECT m.id, m.key, m.content, m.category, m.importance, m.source, m.meta, \
                            m.embedding, m.created_at, m.updated_at, m.access_count, m.last_accessed, m.project_id \
                     FROM memory_links l JOIN memories m ON m.id = l.from_id \
                     WHERE l.to_id = ?1 AND m.deleted_at IS NULL \
                     LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![id, (limit * 2) as i64], |row| {
                    Ok(MemoryRecord {
                        id: row.get(0)?,
                        key: row.get(1)?,
                        content: row.get(2)?,
                        category: row.get(3)?,
                        importance: row.get(4)?,
                        source: row.get(5)?,
                        meta: row.get(6)?,
                        embedding: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                        access_count: row.get(10)?,
                        last_accessed: row.get(11)?,
                        project_id: row.get(12)?,
                    })
                })?;
                for r in rows.flatten() {
                    if seen.insert(r.id) && out.len() < limit {
                        out.push(r);
                    }
                }
            }
            Ok(out)
        })
    }

    /// Затухание важности воспоминаний (decay). Ф23.1: trust гаснет тем же фактором.
    pub fn decay_importance(&self, rate: f64) -> anyhow::Result<usize> {
        let factor = 1.0 - rate.clamp(0.0, 1.0);
        let count = self.db.with_conn(|conn| {
            let count = conn.execute(
                "UPDATE memories SET importance = MAX(0.01, importance * ?1), \
                 trust = MAX(0.0, trust * ?1)",
                params![factor],
            )?;
            Ok(count)
        })?;
        self.mark_forget_candidates()?;
        Ok(count)
    }

    /// Очистка слабых воспоминаний (tombstone, реплицируется синком).
    pub fn purge_weak(&self, threshold: f64, max_access: i64) -> anyhow::Result<usize> {
        let now = utcnow();
        self.db.with_conn(|conn| {
            let count = conn.execute(
                "UPDATE memories SET deleted_at = ?1, updated_at = ?1, origin = ''
                 WHERE importance < ?2 AND access_count < ?3 AND deleted_at IS NULL",
                params![now, threshold, max_access],
            )?;
            Ok(count)
        })
    }

    /// Физическое удаление старых tombstones (maintenance автодрима).
    /// Задержка = 2×retention, чтобы удаление успело уйти пирам через синк.
    pub fn purge_tombstones(&self, older_than_days: i64) -> anyhow::Result<usize> {
        self.db.with_conn(|conn| {
            let count = conn.execute(
                // strftime в формате utcnow() (RFC3339, +00:00) для честного сравнения строк
                "DELETE FROM memories WHERE deleted_at IS NOT NULL AND deleted_at <
                 strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now', ?1)",
                params![format!("-{older_than_days} days")],
            )?;
            Ok(count)
        })
    }

    /// Сборка контекста `<agent_memory>` для инъекции в системный промпт.
    /// Прежний путь (до Фазы 22): топ по importance + подстрочный overlap.
    /// Fallback для пустых/тривиальных запросов и пустого гибридного пула.
    fn build_context_fallback(&self, limit: usize, query: Option<&str>) -> anyhow::Result<String> {
        let query_words: HashSet<String> = query
            .unwrap_or_default()
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        let records = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, key, content, category, importance, source, meta, embedding, created_at, updated_at, access_count, last_accessed, project_id FROM memories WHERE deleted_at IS NULL ORDER BY importance DESC LIMIT 100",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(MemoryRecord {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: row.get(3)?,
                    importance: row.get(4)?,
                    source: row.get(5)?,
                    meta: row.get(6)?,
                    embedding: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    access_count: row.get(10)?,
                    last_accessed: row.get(11)?,
                    project_id: row.get(12)?,
                })
            })?;
            let mut list = Vec::new();
            for r in rows.flatten() {
                list.push(r);
            }
            Ok(list)
        })?;

        let mut scored: Vec<(MemoryRecord, f64)> = records
            .into_iter()
            .map(|r| {
                let overlap = if query_words.is_empty() {
                    0.0
                } else {
                    let content_lower = r.content.to_lowercase();
                    let matches = query_words.iter().filter(|w| content_lower.contains(w.as_str())).count();
                    matches as f64 / query_words.len().max(1) as f64
                };
                let score = 0.6 * r.importance + 0.4 * overlap;
                (r, score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if scored.len() > limit {
            scored.truncate(limit);
        }

        if scored.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::from("<agent_memory>\n");
        for (r, _) in scored {
            out.push_str(&format!("- [{}] {}\n", r.category, r.content));
        }
        out.push_str("</agent_memory>");
        Ok(out)
    }

    /// Контекст prefetch'а (Фаза 22, PLAN_v1.3): непустой запрос → гибридный пул
    /// (FTS5+vector, RRF k=60) → скоринг rel/importance/recency/access → MMR-диверсификация
    /// → бюджет символов по record-границам → touch_access только вошедших записей.
    pub async fn build_context(
        &self,
        limit: usize,
        query: Option<&str>,
        opts: &ContextOptions,
    ) -> anyhow::Result<String> {
        let q = query.map(str::trim).unwrap_or("");
        if q.is_empty() {
            return self.build_context_fallback(limit, query);
        }

        let pool_size = (limit * 2).max(20);
        let hits = match self.search_hybrid_hits(q, pool_size, 0.0, false).await {
            Ok(h) if !h.is_empty() => h,
            _ => return self.build_context_fallback(limit, query),
        };

        // Скоринг (§22.2): 0.35*rel + 0.25*importance + 0.1*trust + 0.1*recency + 0.1*log1p(access).
        // trust — константа 0.1 до Фазы 23. recency — экспоненциальный полураспад по updated_at.
        let max_rrf = hits.iter().map(|h| h.score).fold(0.0_f64, f64::max).max(1e-9);
        let half_life = opts.half_life_days.max(0.1);
        let now = chrono::Utc::now();
        let mut cands: Vec<Candidate> = hits
            .into_iter()
            .map(|h| {
                let rel = (h.score / max_rrf).clamp(0.0, 1.0);
                let age_days = chrono::DateTime::parse_from_rfc3339(&h.record.updated_at)
                    .map(|dt| {
                        now.signed_duration_since(dt.with_timezone(&chrono::Utc))
                            .num_days()
                            .max(0) as f64
                    })
                    .unwrap_or(0.0);
                let recency = (-age_days / half_life).exp();
                // Насыщение доступа: log1p, 1.0 достигается на ~50-м использовании.
                let sat_access =
                    ((1.0 + h.record.access_count.max(0) as f64).ln() / 50f64.ln()).clamp(0.0, 1.0);
                let score =
                    0.35 * rel + 0.25 * h.record.importance + 0.1 + 0.1 * recency + 0.1 * sat_access;
                Candidate { record: h.record, score }
            })
            .collect();
        cands.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // MMR-диверсификация (§22.3): λ — вес релевантности, остаток — разнообразие.
        let max_score = cands.iter().map(|c| c.score).fold(0.0_f64, f64::max).max(1e-9);
        let rels: Vec<f64> = cands.iter().map(|c| c.score / max_score).collect();
        let vecs: Vec<Option<Vec<f32>>> = cands
            .iter()
            .map(|c| c.record.embedding.as_deref().and_then(crate::vector::deserialize))
            .collect();
        let picked = mmr_select(&rels, &vecs, limit, opts.mmr_lambda);

        // Бюджет символов (§22.4): резать по record-границам, не отдавать обрезку Hermes'у.
        let budget = opts.max_chars.unwrap_or(usize::MAX).max(64);
        let block_overhead = "<agent_memory>\n</agent_memory>".chars().count();
        let mut used = block_overhead;
        let mut lines_out: Vec<String> = Vec::new();
        let mut included_ids: Vec<i64> = Vec::new();
        let mut skipped = 0usize;
        for &i in &picked {
            let c = &cands[i];
            let line = format!("- [{}] {}\n", c.record.category, c.record.content);
            let line_chars = line.chars().count();
            if used + line_chars > budget {
                skipped += 1;
                continue;
            }
            used += line_chars;
            included_ids.push(c.record.id);
            lines_out.push(line);
        }
        if lines_out.is_empty() {
            // Бюджет теснее одной записи — жёстко режем первую под бюджет.
            let c = &cands[picked[0]];
            let prefix = format!("- [{}] ", c.record.category);
            let avail = budget
                .saturating_sub(block_overhead + prefix.chars().count() + 1)
                .max(20);
            let content: String = c.record.content.chars().take(avail).collect();
            included_ids.push(c.record.id);
            lines_out.push(format!("- [{}] {}\n", c.record.category, content));
            skipped = picked.len() - 1;
        }
        if skipped > 0 {
            let marker = format!("- …[truncated {skipped} records]\n");
            if used + marker.chars().count() <= budget {
                lines_out.push(marker);
            }
        }

        self.touch_access(&included_ids)?;

        let mut out = String::from("<agent_memory>\n");
        for line in lines_out {
            out.push_str(&line);
        }
        out.push_str("</agent_memory>");
        Ok(out)
    }
}

/// Кандидат контекста: запись + итоговый скоринг (§22.2).
struct Candidate {
    record: MemoryRecord,
    score: f64,
}

/// Параметры сборки контекстного блока (Фаза 22).
#[derive(Debug, Clone)]
pub struct ContextOptions {
    /// Бюджет символов блока (None — без обрезки).
    pub max_chars: Option<usize>,
    /// Полураспад recency в днях.
    pub half_life_days: f64,
    /// Вес релевантности в MMR (0..1), остаток — разнообразие.
    pub mmr_lambda: f64,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self { max_chars: None, half_life_days: 90.0, mmr_lambda: 0.7 }
    }
}

/// MMR-отбор индексов: λ*релевантность − (1−λ)*макс. сходство к уже выбранным.
/// `rel` — нормированные релевантности, `vecs[i]` — эмбеддинг кандидата (None → сходство 0).
pub fn mmr_select(rel: &[f64], vecs: &[Option<Vec<f32>>], limit: usize, lambda: f64) -> Vec<usize> {
    let lambda = lambda.clamp(0.0, 1.0);
    let mut remaining: Vec<usize> = (0..rel.len()).collect();
    let mut selected: Vec<usize> = Vec::new();
    while selected.len() < limit && !remaining.is_empty() {
        let mut best: Option<(usize, f64)> = None;
        for &i in &remaining {
            let max_sim = selected
                .iter()
                .map(|&s| match (&vecs[i], &vecs[s]) {
                    (Some(a), Some(b)) => crate::vector::cosine(a, b) as f64,
                    _ => 0.0,
                })
                .fold(-1.0_f64, f64::max);
            let score = lambda * rel[i] - (1.0 - lambda) * max_sim;
            if best.is_none()
                || score > best.map(|(_, bs)| bs).unwrap_or(f64::NEG_INFINITY)
            {
                best = Some((i, score));
            }
        }
        let (i, _) = best.expect("remaining непуст");
        selected.push(i);
        remaining.retain(|&r| r != i);
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmr_prefers_diverse_when_relevance_close() {
        // v, v (дубли), w (ортогональный). Релевантности близкие.
        let v = vec![1.0f32, 0.0];
        let w = vec![0.0f32, 1.0];
        let rel = [1.0, 0.9, 0.5];
        let vecs = vec![Some(v.clone()), Some(v), Some(w)];
        let picked = mmr_select(&rel, &vecs, 2, 0.7);
        assert_eq!(picked[0], 0, "первым берётся самый релевантный");
        assert_eq!(picked[1], 2, "вторым — разнообразный, а не дубль");
    }

    #[test]
    fn mmr_pure_relevance_when_lambda_one() {
        let rel = [0.5, 1.0];
        let vecs = vec![None::<Vec<f32>>, None];
        let picked = mmr_select(&rel, &vecs, 2, 1.0);
        assert_eq!(picked, vec![1, 0]);
    }
}
