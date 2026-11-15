//! Буферизация телеметрии обращений и обслуживание SQLite WAL (Ф73.3, Ф73.6).
//! - Накопление `access_count` в памяти с пакетным сбросом в БД;
//! - Negative Feedback (`mute_node`): разовый штраф `trust_score *= 0.3` (или `trust *= 0.3`);
//! - `PRAGMA wal_checkpoint(TRUNCATE)` + `PRAGMA optimize` в autodream и при shutdown MCP.

use chrono::Utc;
use rusqlite::params;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::db::Database;
use crate::graph::pagerank::MUTE_TRUST_FACTOR;

/// Порог накопленных событий до автоматического пакетного сброса в БД.
pub const DEFAULT_FLUSH_THRESHOLD: usize = 32;

#[derive(Default)]
struct TelemetryDelta {
    memory_hits: HashMap<i64, u64>,
    graph_hits: HashMap<i64, u64>,
    pending_events: usize,
}

/// Потокобезопасный буфер телеметрии обращений к памяти и графу.
pub struct AccessTelemetryBuffer {
    inner: Mutex<TelemetryDelta>,
    flush_threshold: usize,
}

impl Default for AccessTelemetryBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_FLUSH_THRESHOLD)
    }
}

impl AccessTelemetryBuffer {
    pub fn new(flush_threshold: usize) -> Self {
        Self {
            inner: Mutex::new(TelemetryDelta::default()),
            flush_threshold: flush_threshold.max(1),
        }
    }

    /// Зарегистрировать обращения к записям `memories`.
    pub fn record_memory_hits(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let mut guard = self.inner.lock().unwrap();
        for &id in ids {
            if id > 0 {
                *guard.memory_hits.entry(id).or_insert(0) += 1;
                guard.pending_events += 1;
            }
        }
    }

    /// Зарегистрировать обращения к узлам `graph_nodes`.
    pub fn record_graph_hits(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let mut guard = self.inner.lock().unwrap();
        for &id in ids {
            if id > 0 {
                *guard.graph_hits.entry(id).or_insert(0) += 1;
                guard.pending_events += 1;
            }
        }
    }

    /// Сбросить буфер в БД, если накоплено >= `flush_threshold` событий.
    pub fn maybe_flush(&self, db: &Database) -> anyhow::Result<usize> {
        let should = {
            let guard = self.inner.lock().unwrap();
            guard.pending_events >= self.flush_threshold
        };
        if should {
            self.flush(db)
        } else {
            Ok(0)
        }
    }

    /// Количество ожидающих сброса инкрементов.
    pub fn pending_count(&self) -> usize {
        self.inner.lock().unwrap().pending_events
    }

    /// Принудительный пакетный сброс накопленных `access_count` в БД в одной транзакции.
    pub fn flush(&self, db: &Database) -> anyhow::Result<usize> {
        let (mem_map, graph_map) = {
            let mut guard = self.inner.lock().unwrap();
            if guard.memory_hits.is_empty() && guard.graph_hits.is_empty() {
                guard.pending_events = 0;
                return Ok(0);
            }
            guard.pending_events = 0;
            (
                std::mem::take(&mut guard.memory_hits),
                std::mem::take(&mut guard.graph_hits),
            )
        };

        let now = Utc::now().to_rfc3339();
        let total = mem_map.len() + graph_map.len();
        db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            if !mem_map.is_empty() {
                let mut stmt = tx.prepare_cached(
                    "UPDATE memories SET access_count = access_count + ?1, last_accessed = ?2 WHERE id = ?3",
                )?;
                for (id, delta) in &mem_map {
                    stmt.execute(params![*delta as i64, now, id])?;
                }
            }
            if !graph_map.is_empty() {
                let mut stmt = tx.prepare_cached(
                    "UPDATE graph_nodes SET access_count = access_count + ?1, updated_at = ?2 WHERE id = ?3",
                )?;
                for (id, delta) in &graph_map {
                    stmt.execute(params![*delta as i64, now, id])?;
                }
            }
            tx.commit()?;
            Ok(())
        })?;
        Ok(total)
    }
}

/// Обслуживание SQLite WAL: `PRAGMA wal_checkpoint(TRUNCATE)` + `PRAGMA optimize` (Ф73.3).
/// Вызывается в цикле autodream и при graceful shutdown MCP-сервера.
pub fn wal_maintenance(db: &Database) -> anyhow::Result<(i64, i64)> {
    db.with_conn(|conn| {
        let (busy, log_frames, checkpointed) = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((
                    r.get::<_, i64>(0).unwrap_or(0),
                    r.get::<_, i64>(1).unwrap_or(0),
                    r.get::<_, i64>(2).unwrap_or(0),
                ))
            })
            .unwrap_or((0, 0, 0));
        if busy != 0 {
            tracing::debug!("wal_checkpoint(TRUNCATE): busy={busy}, log={log_frames}, ckpt={checkpointed}");
        }
        conn.execute_batch("PRAGMA optimize;")?;
        Ok((log_frames, checkpointed))
    })
}

/// Negative Feedback / Mute Node (Ф73.2 / Ф73.6):
/// разовый штраф узла графа (`trust_score *= 0.3`) или записи памяти (`trust *= 0.3`).
pub fn mute_graph_node(db: &Database, node_id_or_pk: &str) -> anyhow::Result<Option<f64>> {
    let now = Utc::now().to_rfc3339();
    db.with_conn(|conn| {
        let updated = if let Ok(pk) = node_id_or_pk.parse::<i64>() {
            conn.execute(
                "UPDATE graph_nodes SET trust_score = MAX(0.0, trust_score * ?1), updated_at = ?2 WHERE id = ?3",
                params![MUTE_TRUST_FACTOR, now, pk],
            )?
        } else {
            conn.execute(
                "UPDATE graph_nodes SET trust_score = MAX(0.0, trust_score * ?1), updated_at = ?2 WHERE node_id = ?3",
                params![MUTE_TRUST_FACTOR, now, node_id_or_pk],
            )?
        };
        if updated == 0 {
            return Ok(None);
        }
        let new_trust: f64 = if let Ok(pk) = node_id_or_pk.parse::<i64>() {
            conn.query_row("SELECT trust_score FROM graph_nodes WHERE id = ?1", params![pk], |r| r.get(0))?
        } else {
            conn.query_row("SELECT trust_score FROM graph_nodes WHERE node_id = ?1", params![node_id_or_pk], |r| r.get(0))?
        };
        Ok(Some(new_trust))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_buffer_accumulates_and_flushes_in_batch() {
        let db = Database::in_memory().expect("db");
        let (mem_id, node_id) = db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO memories (key, content, category, created_at, updated_at) \
                     VALUES ('k_tel', 'hello', 'general', datetime('now'), datetime('now'))",
                    [],
                )?;
                let mid = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO graph_nodes (node_id, node_type, label, created_at, updated_at) \
                     VALUES ('fn:tel', 'Function', 'tel', datetime('now'), datetime('now'))",
                    [],
                )?;
                let nid = conn.last_insert_rowid();
                Ok((mid, nid))
            })
            .expect("seed");

        let buf = AccessTelemetryBuffer::new(4);
        buf.record_memory_hits(&[mem_id, mem_id]);
        buf.record_graph_hits(&[node_id]);
        assert_eq!(buf.pending_count(), 3);
        // Ниже порога — сброса нет
        assert_eq!(buf.maybe_flush(&db).unwrap(), 0);

        // Достигаем порога 4
        buf.record_graph_hits(&[node_id]);
        assert_eq!(buf.maybe_flush(&db).unwrap(), 2);
        assert_eq!(buf.pending_count(), 0);

        let (m_acc, g_acc): (i64, i64) = db
            .with_conn(|conn| {
                let m: i64 = conn.query_row(
                    "SELECT access_count FROM memories WHERE id = ?1",
                    params![mem_id],
                    |r| r.get(0),
                )?;
                let g: i64 = conn.query_row(
                    "SELECT access_count FROM graph_nodes WHERE id = ?1",
                    params![node_id],
                    |r| r.get(0),
                )?;
                Ok((m, g))
            })
            .expect("query");
        assert_eq!(m_acc, 2);
        assert_eq!(g_acc, 2);
    }

    #[test]
    fn test_wal_maintenance_and_mute_node() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("wal_test.db")).expect("db");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO graph_nodes (node_id, node_type, label, created_at, updated_at) \
                 VALUES ('fn:noisy', 'Function', 'noisy', datetime('now'), datetime('now'))",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let new_trust = mute_graph_node(&db, "fn:noisy").unwrap().unwrap();
        assert!((new_trust - 0.3).abs() < 1e-9);

        let _ = wal_maintenance(&db).expect("wal_maintenance");
    }
}
