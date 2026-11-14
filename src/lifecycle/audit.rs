//! Аудит-леджер (M11, Фазы 64/68): append-only журнал операций над памятью,
//! уроками, слотами и секретами. В бандлы синка не входит (SYNC.md).
//! Единая точка записи — `record()`; просмотр — `list()`; ротация — `prune_weekly()`.
//! Ошибки записи — rusqlite::Result, чтобы работать внутри `Database::with_conn`;
//! просмотр/ротация — anyhow (могут зваться напрямую).
//! Значения секретов сюда не пишутся никогда — только имена и метаданные (ADR-21/22).

use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;

use crate::db::Database;

fn json_str<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

/// Запись леджера (сериализуется для MCP memory_audit).
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: String,
    pub actor: String,
    pub action: String,
    pub target_ids: Vec<String>,
    pub details: Value,
    pub source_tool: Option<String>,
}

/// Записать событие в аудит-леджер. `targets` — идентификаторы затронутых записей;
/// `details` — произвольный JSON (причины, размеры, счётчики).
pub fn record(
    conn: &Connection,
    actor: &str,
    action: &str,
    targets: &[String],
    details: serde_json::Value,
    source_tool: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO audit_log (ts, actor, action, target_ids, details, source_tool)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            crate::db::utcnow(),
            actor,
            action,
            json_str(targets),
            json_str(&details),
            source_tool,
        ],
    )?;
    Ok(())
}

/// Просмотр леджера (Ф68.2): последние записи, фильтр по action.
pub fn list(db: &Database, action: Option<&str>, limit: usize) -> anyhow::Result<Vec<AuditEntry>> {
    db.with_conn(|conn| {
        let sql = "SELECT id, ts, actor, action, target_ids, details, source_tool \
                   FROM audit_log WHERE (?1 IS NULL OR action = ?1) ORDER BY id DESC LIMIT ?2";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![action, limit as i64], |r| {
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                actor: r.get(2)?,
                action: r.get(3)?,
                target_ids: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                details: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                source_tool: r.get(6)?,
            })
        })?;
        Ok(rows.flatten().collect())
    })
}

/// Разовая ротация (Ф68.3): удалить записи старше `days` дней. Порог считается
/// в том же формате RFC3339, что и `ts` (utcnow), — сравнение строк корректно.
pub fn prune(db: &Database, days: i64) -> anyhow::Result<usize> {
    db.with_conn(|conn| {
        let threshold: String = conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
            params![format!("-{days} days")],
            |r| r.get(0),
        )?;
        Ok(conn.execute(
            "DELETE FROM audit_log WHERE ts < ?1",
            params![threshold],
        )?)
    })
}

/// Недельная ротация с курсором kv (Ф68.3) — из autodream-регламента.
/// None = «неделя ещё не прошла». В леджере только id-шники и метаданные,
/// персональный контент не хранится — ротация безопасна.
pub fn prune_weekly(db: &Database, days: i64) -> anyhow::Result<Option<usize>> {
    const CURSOR: &str = "audit_prune_last_ts";
    let now = chrono::Utc::now();
    if let Some(ts) = db.get_kv(CURSOR)? {
        if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&ts) {
            if (now - t.with_timezone(&chrono::Utc)).num_days() < 7 {
                return Ok(None);
            }
        }
    }
    let n = prune(db, days)?;
    db.set_kv(CURSOR, &crate::db::utcnow())?;
    Ok(Some(n))
}
