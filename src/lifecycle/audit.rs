//! Аудит-леджер (M11, Фаза 64/68): append-only журнал операций над памятью,
//! уроками, слотами и секретами. В бандлы синка не входит (SYNC.md).
//! Ф68 расширит инструментирование и просмотр; здесь — единая точка записи.
//! Ошибки — rusqlite::Result, чтобы запись работала внутри `Database::with_conn`.

use rusqlite::{params, Connection};

fn json_str<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

/// Записать событие в аудит-леджер. `targets` — идентификаторы затронутых записей;
/// `details` — произвольный JSON (причины, размеры, счётчики). Значения секретов
/// сюда не пишутся никогда — только имена и метаданные (ADR-21/22).
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
