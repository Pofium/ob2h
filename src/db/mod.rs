//! Подключение к SQLite, потокобезопасность, вспомогательные методы.

pub mod models;
pub mod schema;

use std::path::Path;
use std::sync::{Arc, Mutex};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Result};

pub use models::*;

pub fn utcnow() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn new<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(p)?;
        // Mode B (плагин + mcp_servers одновременно): два процесса на одной БД,
        // WAL-режим — даём SQLite ждать блокировку вместо мгновенной ошибки.
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;

        // Живая БД старой схемы (< v3) перед миграцией — снапшот в backups/
        let version = schema::schema_version(&conn);
        if version > 0 && version < 3 {
            let backup_dir = p.parent().unwrap_or_else(|| Path::new(".")).join("backups");
            let _ = std::fs::create_dir_all(&backup_dir);
            let ts = Utc::now().format("%Y%m%d-%H%M%S");
            let backup_path = if version == 1 {
                backup_dir.join(format!("pre-v08-m2-{ts}.db"))
            } else {
                backup_dir.join(format!("pre-v10-m3-{ts}.db"))
            };
            let escaped = backup_path.to_string_lossy().replace('\'', "''");
            if conn.execute_batch(&format!("VACUUM INTO '{escaped}'")).is_ok() {
                tracing::info!("Бэкап перед миграцией: {}", backup_path.display());
            }
        }

        schema::migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn conn_arc(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn with_conn<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&mut Connection) -> Result<R>,
    {
        let mut lock = self.conn.lock().map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        Ok(f(&mut lock)?)
    }

    /// Транзакция с anyhow-ошибками: f получает &Transaction, коммит после успеха,
    /// откат при Err.
    pub fn with_tx<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&rusqlite::Transaction) -> anyhow::Result<R>,
    {
        let mut lock = self.conn.lock().map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        let tx = lock.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn get_kv(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM kv WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
        })
    }

    pub fn set_kv(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_db_migrations() {
        let db = Database::in_memory().expect("db in memory must initialize");
        let version = db.get_kv("schema_version").expect("get_kv").expect("must have version");
        assert_eq!(version, crate::db::schema::SCHEMA_VERSION.to_string());

        db.set_kv("test_key", "test_val").expect("set_kv");
        assert_eq!(db.get_kv("test_key").expect("get_kv").unwrap(), "test_val");
    }
}
