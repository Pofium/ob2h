//! Уроки (lessons, Фаза 64): процедурный опыт «что делать / чего избегать /
//! когда применимо» с показателем уверенности. Семантика по мотивам
//! rohitg00/agentmemory (см. PLAN_v1.9.9 §3.2), адаптирована под OB2H:
//! дедуп по fingerprint контента, reinforce при повторе, недельный decay
//! в дриме, tombstone вместо auto-soft-delete (ADR-11 — никаких автоудалений).
//!
//! Внутренние запросы живут в мире `rusqlite::Result` (контракт `with_conn`),
//! публичный API — `anyhow::Result` с ошибками `[Error] … [Подсказка]`.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::audit;
use crate::db::{utcnow, Database};

/// Шаг укрепления: `conf' = min(1.0, conf + 0.1·(1−conf))`.
pub const REINFORCE_STEP: f64 = 0.1;
/// Пол уверенности: ниже не затухает.
pub const CONFIDENCE_FLOOR: f64 = 0.05;
/// Порог кандидата на забывание (не удалять! только пометка в отчёте).
pub const CANDIDATE_THRESHOLD: f64 = 0.10;
/// Кап на источник цитат у одного урока.
pub const SOURCE_IDS_CAP: usize = 50;
/// Делитель recency-буста: `1 / (1 + дней_с_укрепления · 0.01)`.
pub const RECENCY_DIVISOR: f64 = 0.01;

const KV_DECAY_CURSOR: &str = "lessons_decay_last_ts";
const DECAY_INTERVAL_DAYS: i64 = 7;
/// Порядок колонок — на него опирается `row_to_lesson` (индексы 0..14).
const LESSON_COLUMNS: &str = "seq, id, content, context, confidence, reinforcements, source, \
     source_ids, project, tags, decay_rate, created_at, updated_at, \
     last_reinforced_at, last_decayed_at";
/// Для JOIN с lessons_fts — квалифицированный вариант того же списка.
const LESSON_COLUMNS_L: &str = "l.seq, l.id, l.content, l.context, l.confidence, l.reinforcements, \
     l.source, l.source_ids, l.project, l.tags, l.decay_rate, l.created_at, l.updated_at, \
     l.last_reinforced_at, l.last_decayed_at";

#[derive(Debug, Clone, Serialize)]
pub struct LessonRecord {
    pub id: String,
    pub content: String,
    pub context: String,
    pub confidence: f64,
    pub reinforcements: i64,
    pub source: String,
    pub source_ids: Vec<String>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub decay_rate: f64,
    pub created_at: String,
    pub updated_at: String,
    pub last_reinforced_at: Option<String>,
    pub last_decayed_at: Option<String>,
    /// Скор recall (confidence × relevance × recency), отсутствует в list/get.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct LessonSaveRequest {
    pub content: String,
    pub context: Option<String>,
    pub confidence: Option<f64>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    /// manual | consolidation | recap | dream
    pub source: Option<String>,
    pub source_ids: Vec<String>,
    pub actor: String,
    pub tool: String,
}

#[derive(Debug, Serialize)]
pub struct LessonSaveOutcome {
    /// created | strengthened
    pub action: String,
    pub lesson: LessonRecord,
}

pub struct LessonsService {
    db: Database,
}

/// Детерминированный дедуп-ключ: `lsn_` + sha256(trim+lowercase)[0..16 hex].
pub fn fingerprint(content: &str) -> String {
    let normalized = content.trim().to_lowercase();
    let hash = Sha256::digest(normalized.as_bytes());
    format!("lsn_{}", hex::encode(&hash[..8]))
}

fn json_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// RFC3339 (наши записи) либо sqlite datetime('now') (фикстуры/миграции).
fn parse_ts(s: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|n| n.and_utc())
        })
}

fn recency_boost(
    last_reinforced_at: Option<&str>,
    created_at: &str,
    now: chrono::DateTime<Utc>,
) -> f64 {
    let baseline = last_reinforced_at
        .and_then(parse_ts)
        .or_else(|| parse_ts(created_at));
    match baseline {
        Some(t) => {
            let days = (now - t).num_seconds() as f64 / 86_400.0;
            1.0 / (1.0 + days.max(0.0) * RECENCY_DIVISOR)
        }
        None => 1.0,
    }
}

impl LessonsService {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Маппинг строки (колонки в порядке LESSON_COLUMNS; seq на позиции 0 игнорируется).
    fn row_to_lesson(row: &rusqlite::Row<'_>) -> rusqlite::Result<LessonRecord> {
        Ok(LessonRecord {
            id: row.get(1)?,
            content: row.get(2)?,
            context: row.get(3)?,
            confidence: row.get(4)?,
            reinforcements: row.get(5)?,
            source: row.get(6)?,
            source_ids: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
            project: row.get(8)?,
            tags: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
            decay_rate: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
            last_reinforced_at: row.get(13)?,
            last_decayed_at: row.get(14)?,
            score: None,
        })
    }

    fn get_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<LessonRecord>> {
        let sql =
            format!("SELECT {LESSON_COLUMNS} FROM lessons WHERE id = ?1 AND deleted_at IS NULL");
        conn.query_row(&sql, params![id], Self::row_to_lesson)
            .optional()
    }

    /// Сохранить урок. Повторное сохранение того же контента укрепляет существующий
    /// (без дубля): confidence растёт по формуле насыщения, source_ids мержатся (кап 50).
    pub fn save(&self, req: LessonSaveRequest) -> anyhow::Result<LessonSaveOutcome> {
        let content = req.content.trim();
        if content.is_empty() {
            anyhow::bail!("[Error] content урока пуст [Подсказка: напишите, что делать / чего избегать / когда применимо]");
        }
        if content.chars().count() > 10_000 {
            anyhow::bail!("[Error] content урока превышает 10 000 символов [Подсказка: сократите урок до сути]");
        }
        let source = req.source.as_deref().unwrap_or("manual");
        if !matches!(source, "manual" | "consolidation" | "recap" | "dream") {
            anyhow::bail!("[Error] недопустимый source \"{source}\" [Подсказка: manual | consolidation | recap | dream]");
        }
        let confidence = match req.confidence {
            Some(c) if !(0.0..=1.0).contains(&c) => {
                anyhow::bail!("[Error] confidence должен быть в диапазоне 0.0–1.0, получено {c}")
            }
            Some(c) => c,
            None => 0.5,
        };

        let fp = fingerprint(content);
        let now = utcnow();

        self.db.with_conn(|conn| {
            let existing: Option<i64> = conn
                .query_row(
                    "SELECT seq FROM lessons WHERE id = ?1 AND deleted_at IS NULL",
                    params![fp],
                    |r| r.get(0),
                )
                .optional()?;

            if existing.is_some() {
                // Reinforce: дубли не создаём (ADR-11 совместимо — запись одна).
                let (conf, reinforcements, old_context, old_sources): (
                    f64,
                    i64,
                    String,
                    String,
                ) = conn.query_row(
                    "SELECT confidence, reinforcements, context, source_ids FROM lessons WHERE id = ?1",
                    params![fp],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                let new_conf = round3((conf + REINFORCE_STEP * (1.0 - conf)).min(1.0));

                let mut merged: Vec<String> =
                    serde_json::from_str(&old_sources).unwrap_or_default();
                for sid in &req.source_ids {
                    if !merged.contains(sid) {
                        merged.push(sid.clone());
                    }
                }
                if merged.len() > SOURCE_IDS_CAP {
                    merged = merged[merged.len() - SOURCE_IDS_CAP..].to_vec();
                }

                let ctx = if old_context.trim().is_empty() {
                    req.context.as_deref().unwrap_or("").trim().to_string()
                } else {
                    old_context
                };
                conn.execute(
                    "UPDATE lessons SET confidence = ?1, reinforcements = ?2, context = ?3, \
                     source_ids = ?4, last_reinforced_at = ?5, updated_at = ?5 WHERE id = ?6",
                    params![new_conf, reinforcements + 1, ctx, json_str(&merged), now, fp],
                )?;
                audit::record(
                    conn,
                    &req.actor,
                    "lesson_strengthen",
                    &[fp.clone()],
                    serde_json::json!({ "from": conf, "to": new_conf }),
                    &req.tool,
                )?;
                let lesson =
                    Self::get_by_id(conn, &fp)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                Ok(LessonSaveOutcome {
                    action: "strengthened".to_string(),
                    lesson,
                })
            } else {
                conn.execute(
                    "INSERT INTO lessons (id, content, context, confidence, reinforcements, source, \
                     source_ids, project, tags, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8, ?9, ?9)",
                    params![
                        fp,
                        content,
                        req.context.as_deref().unwrap_or("").trim(),
                        confidence,
                        source,
                        json_str(&req.source_ids),
                        req.project,
                        json_str(&req.tags),
                        now
                    ],
                )?;
                audit::record(
                    conn,
                    &req.actor,
                    "lesson_save",
                    &[fp.clone()],
                    serde_json::json!({ "confidence": confidence, "source": source }),
                    &req.tool,
                )?;
                let lesson =
                    Self::get_by_id(conn, &fp)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                Ok(LessonSaveOutcome {
                    action: "created".to_string(),
                    lesson,
                })
            }
        })
    }

    /// Поиск уроков: FTS5 (bm25) × confidence × recency. Уроки с confidence ниже
    /// `min_confidence` и уроки чужого проекта отфильтровываются. Сортировка по
    /// score desc, тай-брейк по id (детерминизм, Ф54).
    pub fn recall(
        &self,
        query: &str,
        project: Option<&str>,
        min_confidence: f64,
        limit: usize,
        actor: &str,
        tool: &str,
    ) -> anyhow::Result<Vec<LessonRecord>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 100);
        // Глубже выборка при фильтрации — иначе пост-фильтры съедают результат
        // (приём из agentmemory lessons-recall).
        let filtering = project.is_some() || min_confidence > CANDIDATE_THRESHOLD;
        let fetch = if filtering {
            (limit * 10).max(100)
        } else {
            (limit * 5).max(50)
        };

        let mut hits = self.recall_fts(query, fetch)?;
        if hits.is_empty() {
            // FTS trigram не обязан матчить короткие/нестандартные запросы —
            // мягкий фолбэк на LIKE, чтобы recall не был пустее memories.
            hits = self.recall_like(query, fetch)?;
        }

        let now = Utc::now();
        let best = hits.iter().map(|(_, rank)| *rank).fold(0.0_f64, f64::max);
        let mut scored: Vec<LessonRecord> = if best > 0.0 {
            hits.into_iter()
                .map(|(mut l, rank)| {
                    let relevance = rank / best;
                    let boost = recency_boost(l.last_reinforced_at.as_deref(), &l.created_at, now);
                    let score = l.confidence * relevance * boost;
                    l.score = Some(round3(score));
                    l
                })
                .filter(|l| l.confidence >= min_confidence)
                .filter(|l| match project {
                    Some(p) => l.project.as_deref() == Some(p),
                    None => true,
                })
                .collect()
        } else {
            Vec::new()
        };
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        scored.truncate(limit);

        self.db.with_conn(|conn| {
            audit::record(
                conn,
                actor,
                "lesson_recall",
                &[],
                serde_json::json!({ "query": query, "resultCount": scored.len() }),
                tool,
            )?;
            Ok(())
        })?;
        Ok(scored)
    }

    /// FTS-хиты: (урок, релевантность). bm25 в FTS5 отрицательный (меньше = лучше) —
    /// инвертируем; нормирование на топ-хит — в recall().
    fn recall_fts(&self, query: &str, fetch: usize) -> anyhow::Result<Vec<(LessonRecord, f64)>> {
        self.db.with_conn(|conn| {
            let sql = format!(
                "SELECT {LESSON_COLUMNS_L}, bm25(lessons_fts) \
                 FROM lessons_fts JOIN lessons l ON l.seq = lessons_fts.rowid \
                 WHERE lessons_fts MATCH ?1 AND l.deleted_at IS NULL \
                 ORDER BY bm25(lessons_fts) LIMIT ?2"
            );
            let mut out = Vec::new();
            match conn.prepare(&sql) {
                Ok(mut stmt) => {
                    let rows = stmt.query_map(params![query, fetch as i64], |r| {
                        let l = Self::row_to_lesson(r)?;
                        let bm: f64 = r.get(15)?;
                        Ok((l, -bm))
                    })?;
                    for r in rows.flatten() {
                        out.push(r);
                    }
                }
                // Невалидный MATCH-синтаксис или отсутствие индекса → пусто,
                // recall() уйдёт в LIKE-фолбэк (как search_fts в memory).
                Err(_) => {}
            }
            Ok(out)
        })
    }

    fn recall_like(&self, query: &str, fetch: usize) -> anyhow::Result<Vec<(LessonRecord, f64)>> {
        let like = format!("%{}%", query.replace('%', "").replace('_', ""));
        self.db.with_conn(|conn| {
            let sql = format!(
                "SELECT {LESSON_COLUMNS} FROM lessons \
                 WHERE (content LIKE ?1 OR context LIKE ?1) AND deleted_at IS NULL LIMIT ?2"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![like, fetch as i64], |r| {
                let l = Self::row_to_lesson(r)?;
                Ok((l, 1.0))
            })?;
            let mut out = Vec::new();
            for r in rows.flatten() {
                out.push(r);
            }
            Ok(out)
        })
    }

    /// Список уроков (по confidence desc), tombstones скрыты.
    pub fn list(
        &self,
        project: Option<&str>,
        source: Option<&str>,
        min_confidence: f64,
        limit: usize,
    ) -> anyhow::Result<Vec<LessonRecord>> {
        let limit = limit.clamp(1, 200);
        self.db.with_conn(|conn| {
            let sql = format!(
                "SELECT {LESSON_COLUMNS} FROM lessons \
                 WHERE deleted_at IS NULL \
                 AND (?1 IS NULL OR project = ?1) \
                 AND (?2 IS NULL OR source = ?2) \
                 AND confidence >= ?3 \
                 ORDER BY confidence DESC, updated_at DESC LIMIT ?4"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![project, source, min_confidence, limit as i64],
                Self::row_to_lesson,
            )?;
            let mut out = Vec::new();
            for r in rows.flatten() {
                out.push(r);
            }
            Ok(out)
        })
    }

    /// Мягкое удаление (tombstone `deleted_at`) — только по явной команде (ADR-11).
    /// Возврат false — урок не найден или уже удалён.
    pub fn delete(&self, lesson_id: &str, actor: &str, tool: &str) -> anyhow::Result<bool> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE lessons SET deleted_at = ?1, updated_at = ?1 \
                 WHERE id = ?2 AND deleted_at IS NULL",
                params![utcnow(), lesson_id],
            )?;
            if changed > 0 {
                audit::record(
                    conn,
                    actor,
                    "lesson_delete",
                    &[lesson_id.to_string()],
                    serde_json::json!({}),
                    tool,
                )?;
            }
            Ok(changed > 0)
        })
    }

    /// Недельный decay-sweep (вызывается из дрима/CLI): уроки «в покое» ≥1 недели
    /// теряют `decay_rate × недель` уверенности (пол 0.05). НИЧЕГО НЕ УДАЛЯЕТ:
    /// уроки с conf ≤ 0.10 и reinforcements == 0 считаются кандидатами в отчёте.
    /// Курсор в kv — шаг раз в `DECAY_INTERVAL_DAYS` дней.
    pub fn decay_sweep(&self, actor: &str) -> anyhow::Result<serde_json::Value> {
        let now = Utc::now();
        let cursor = self.db.get_kv(KV_DECAY_CURSOR)?;
        if let Some(ts) = cursor.as_deref().and_then(parse_ts) {
            if (now - ts).num_days() < DECAY_INTERVAL_DAYS {
                return Ok(serde_json::json!({
                    "skipped": true,
                    "reason": "decay-sweep выполняется раз в 7 дней",
                    "last_sweep": ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                }));
            }
        }

        let now_str = utcnow();
        let (decayed, total) = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT seq, confidence, decay_rate, \
                 COALESCE(last_decayed_at, last_reinforced_at, created_at) \
                 FROM lessons WHERE deleted_at IS NULL",
            )?;
            let rows: Vec<(i64, f64, f64, String)> = stmt
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .flatten()
                .collect();

            let mut decayed = 0usize;
            for (seq, conf, rate, baseline) in rows {
                let Some(b) = parse_ts(&baseline) else { continue };
                let weeks = (now - b).num_seconds() as f64 / (7.0 * 86_400.0);
                if weeks < 1.0 {
                    continue;
                }
                let new_conf = round3((conf - rate * weeks).max(CONFIDENCE_FLOOR));
                if (new_conf - conf).abs() < 1e-9 {
                    continue;
                }
                conn.execute(
                    "UPDATE lessons SET confidence = ?1, last_decayed_at = ?2, updated_at = ?2 \
                     WHERE seq = ?3",
                    params![new_conf, now_str, seq],
                )?;
                decayed += 1;
            }

            let total: i64 = conn.query_row(
                "SELECT COUNT(*) FROM lessons WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            Ok((decayed, total))
        })?;

        let candidates: i64 = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM lessons \
                 WHERE deleted_at IS NULL AND confidence <= ?1 AND reinforcements = 0",
                params![CANDIDATE_THRESHOLD],
                |r| r.get(0),
            )
        })?;

        self.db.set_kv(KV_DECAY_CURSOR, &utcnow())?;
        self.db.with_conn(|conn| {
            audit::record(
                conn,
                actor,
                "lesson_decay",
                &[],
                serde_json::json!({ "decayed": decayed, "candidates": candidates }),
                "decay_sweep",
            )
        })?;

        Ok(serde_json::json!({
            "swept": true,
            "decayed": decayed,
            "candidates_for_forget": candidates,
            "total_live": total,
            "swept_at": now_str,
        }))
    }
}
