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

    /// Сохранить или обновить воспоминание.
    pub async fn save(
        &self,
        content: &str,
        key: Option<&str>,
        category: &str,
        importance: f64,
        source: &str,
        meta: Option<&str>,
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
                    key, content, category, importance, source, meta, embedding, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                ON CONFLICT(key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    importance = excluded.importance,
                    source = excluded.source,
                    meta = excluded.meta,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at
                "#,
                params![k, content, category, importance, source, meta, emb_blob, now],
            )?;
            Ok(())
        })?;

        Ok(k)
    }

    /// Получить воспоминание по ключу.
    pub fn get(&self, key: &str) -> anyhow::Result<Option<MemoryRecord>> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, key, content, category, importance, source, meta, embedding, created_at, updated_at, access_count, last_accessed FROM memories WHERE key = ?1",
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
                SET content = ?1, importance = ?2, category = ?3, embedding = ?4, updated_at = ?5
                WHERE key = ?6
                "#,
                params![new_content, new_importance, new_category, emb_blob, now, key],
            )?;
            Ok(())
        })?;

        Ok(true)
    }

    /// Удалить воспоминание по ключу.
    pub fn forget(&self, key: &str) -> anyhow::Result<bool> {
        self.db.with_conn(|conn| {
            let count = conn.execute("DELETE FROM memories WHERE key = ?1", params![key])?;
            Ok(count > 0)
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
                        "SELECT id, importance FROM memories WHERE content LIKE ?1 LIMIT ?2",
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
            let mut stmt = conn.prepare("SELECT id, embedding FROM memories WHERE embedding IS NOT NULL")?;
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

    /// Гибридный поиск: FTS5 + Vector слияние через RRF (k=60).
    pub async fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        min_score: f32,
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

        // Обновляем счетчик обращений
        let ids: Vec<i64> = hits.iter().map(|h| h.record.id).collect();
        self.touch_access(&ids)?;

        Ok(hits)
    }

    pub fn get_by_id(&self, id: i64) -> anyhow::Result<Option<MemoryRecord>> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, key, content, category, importance, source, meta, embedding, created_at, updated_at, access_count, last_accessed FROM memories WHERE id = ?1",
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
                conn.execute(
                    "UPDATE memories SET access_count = access_count + 1, last_accessed = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
            Ok(())
        })
    }

    /// Затухание важности воспоминаний (decay).
    pub fn decay_importance(&self, rate: f64) -> anyhow::Result<usize> {
        let factor = 1.0 - rate.clamp(0.0, 1.0);
        self.db.with_conn(|conn| {
            let count = conn.execute(
                "UPDATE memories SET importance = MAX(0.01, importance * ?1)",
                params![factor],
            )?;
            Ok(count)
        })
    }

    /// Очистка слабых воспоминаний (purge).
    pub fn purge_weak(&self, threshold: f64, max_access: i64) -> anyhow::Result<usize> {
        self.db.with_conn(|conn| {
            let count = conn.execute(
                "DELETE FROM memories WHERE importance < ?1 AND access_count < ?2",
                params![threshold, max_access],
            )?;
            Ok(count)
        })
    }

    /// Сборка контекста `<agent_memory>` для инъекции в системный промпт.
    pub fn build_context(&self, limit: usize, query: Option<&str>) -> anyhow::Result<String> {
        let query_words: HashSet<String> = query
            .unwrap_or_default()
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        let records = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, key, content, category, importance, source, meta, embedding, created_at, updated_at, access_count, last_accessed FROM memories ORDER BY importance DESC LIMIT 100",
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
}
