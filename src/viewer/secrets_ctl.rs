//! Контроллер таба «Секреты» (Ф71): состояния not_initialized/locked/unlocked
//! поверх переиспользованного `SecretsService` — своей криптографии нет.
//!
//! Единственное санкционированное исключение из read-only (§5.5): RW-соединение
//! создаётся лениво при первом действии пользователя и пишет только в
//! `secrets`/`kv`/`audit_log`. Аудит — actor=user, source_tool=viewer, только
//! имена и размеры, значения не попадают в леджер никогда (ADR-21/22).
//! Неверный PIN не пишет ничего (ни данных, ни аудита).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::json;
use zeroize::Zeroizing;

use crate::secrets::pin_input::{is_valid_pin, PIN_FORMAT_ERROR};
use crate::secrets::service::{SecretRecordMeta, SecretsService};

/// Состояние сейфа для UI (Ф71.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretsState {
    NotInitialized,
    Locked,
    Unlocked { ttl_left_sec: u64 },
}

/// Контроллер секретов viewer: ленивое RW-соединение + аудит операций.
pub struct SecretsController {
    db_path: PathBuf,
    ttl_secs: u64,
    service: Option<SecretsService>,
    conn: Option<Arc<Mutex<Connection>>>,
}

impl SecretsController {
    pub fn new(db_path: &Path, ttl_secs: u64) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
            ttl_secs,
            service: None,
            conn: None,
        }
    }

    /// Лениво открывает RW-соединение и создаёт сервис (только по действию пользователя).
    fn ensure(&mut self) -> anyhow::Result<&SecretsService> {
        if self.service.is_none() {
            let conn = Connection::open(&self.db_path)?;
            conn.busy_timeout(Duration::from_millis(5000))?;
            let arc = Arc::new(Mutex::new(conn));
            self.service = Some(SecretsService::new(arc.clone(), self.ttl_secs));
            self.conn = Some(arc);
        }
        Ok(self.service.as_ref().unwrap())
    }

    /// Состояние сейфа. `ro` — read-only соединение viewer: статус читается без
    /// открытия RW (окно может годами висеть на табе «Секреты» без единой записи).
    pub fn state(&self, ro: &Connection) -> SecretsState {
        let initialized: bool = ro
            .query_row(
                "SELECT value FROM kv WHERE key = 'secrets.initialized'",
                [],
                |r| r.get::<_, String>(0),
            )
            .map(|v| v == "1")
            .unwrap_or(false);
        if !initialized {
            return SecretsState::NotInitialized;
        }
        if let Some(svc) = &self.service {
            // status() сам гасит протухшую сессию (zeroize ключа по TTL).
            if let Ok(st) = svc.status() {
                if !st.locked {
                    return SecretsState::Unlocked {
                        ttl_left_sec: st.ttl_left_sec.unwrap_or(0),
                    };
                }
            }
        }
        SecretsState::Locked
    }

    fn audit(&self, action: &str, targets: &[String], details: serde_json::Value) {
        if let Some(conn) = &self.conn {
            if let Ok(c) = conn.lock() {
                let _ = crate::lifecycle::audit::record(&c, "user", action, targets, details, "viewer");
            }
        }
    }

    /// Инициализация сейфа: PIN дважды, повторная инициализация запрещена (Ф71.1).
    pub fn init(&mut self, pin: &str, confirm: &str) -> anyhow::Result<()> {
        if !is_valid_pin(pin) {
            anyhow::bail!("[Error] {PIN_FORMAT_ERROR} [Подсказка] ровно 4 цифры");
        }
        if pin != confirm {
            anyhow::bail!("[Error] PIN-коды не совпадают [Подсказка] введите одинаковые значения в оба поля");
        }
        if self.ensure()?.is_initialized()? {
            anyhow::bail!(
                "[Error] хранилище секретов уже инициализировано \
                 [Подсказка] повторная инициализация запрещена — разблокируйте сейф существующим PIN"
            );
        }
        self.ensure()?.init(pin, None)?;
        self.audit("secrets_init", &[], json!({}));
        Ok(())
    }

    /// Разблокировка PIN-ом. Неверный PIN — ошибка без единой записи (Ф71.3).
    pub fn unlock(&mut self, pin: &str) -> anyhow::Result<()> {
        self.ensure()?.unlock(pin)?;
        self.audit("secrets_unlock", &[], json!({}));
        Ok(())
    }

    /// Блокировка: ключ затирается (Zeroizing внутри SecretsService).
    pub fn lock(&mut self) {
        if let Some(svc) = &self.service {
            svc.lock();
            self.audit("secrets_lock", &[], json!({}));
        }
    }

    /// Сохранить секрет. В аудит — имя и размер, не значение.
    pub fn set(&mut self, name: &str, value: &str, hint: Option<&str>) -> anyhow::Result<()> {
        self.ensure()?.set(name, value, hint)?;
        self.audit(
            "secrets_set",
            &[name.to_string()],
            json!({ "name": name, "size": value.len() }),
        );
        Ok(())
    }

    /// Расшифровка значения (обновляет access_count — санкционированная запись).
    pub fn reveal(&mut self, name: &str) -> anyhow::Result<Zeroizing<String>> {
        let value = self.ensure()?.get(name)?;
        self.audit("secrets_get", &[name.to_string()], json!({ "name": name }));
        Ok(value)
    }

    /// Удаление секрета.
    pub fn delete(&mut self, name: &str) -> anyhow::Result<bool> {
        let removed = self.ensure()?.delete(name)?;
        if removed {
            self.audit("secrets_delete", &[name.to_string()], json!({ "name": name }));
        }
        Ok(removed)
    }

    /// Метаданные секретов без значений (требует разблокировки).
    pub fn list(&mut self) -> anyhow::Result<Vec<SecretRecordMeta>> {
        self.ensure()?.list()
    }

    /// Закрытие окна/блокировка: затирание ключа и освобождение RW-соединения.
    pub fn shutdown(&mut self) {
        self.lock();
        self.service = None;
        self.conn = None;
    }
}

impl Drop for SecretsController {
    fn drop(&mut self) {
        // Ф71.3: ключ не переживает закрытие окна.
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup(ttl: u64) -> (tempfile::TempDir, Database, SecretsController) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::new(tmp.path().join("ob2h.db")).unwrap();
        let ctl = SecretsController::new(&tmp.path().join("ob2h.db"), ttl);
        (tmp, db, ctl)
    }

    fn ro(tmp: &tempfile::TempDir) -> Connection {
        super::super::data::open_readonly(&tmp.path().join("ob2h.db")).unwrap()
    }

    fn audit_rows(db: &Database) -> Vec<(String, String, String)> {
        db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT action, details, source_tool FROM audit_log ORDER BY id")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap()
    }

    #[test]
    fn full_cycle_init_unlock_set_reveal_delete_lock() {
        let (tmp, db, mut ctl) = setup(1800);
        assert_eq!(ctl.state(&ro(&tmp)), SecretsState::NotInitialized);
        ctl.init("1234", "1234").unwrap();
        assert_eq!(ctl.state(&ro(&tmp)), SecretsState::Locked);
        ctl.unlock("1234").unwrap();
        assert!(matches!(ctl.state(&ro(&tmp)), SecretsState::Unlocked { .. }));
        ctl.set("api.key", "super-secret-value", Some("hint")).unwrap();
        let list = ctl.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "api.key");
        let v = ctl.reveal("api.key").unwrap();
        assert_eq!(v.as_str(), "super-secret-value");
        assert!(ctl.delete("api.key").unwrap());
        ctl.lock();
        assert_eq!(ctl.state(&ro(&tmp)), SecretsState::Locked);
        assert!(ctl.reveal("api.key").is_err());

        // Аудит: actor=user, source_tool=viewer, значения не утекают.
        let rows = audit_rows(&db);
        let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            actions,
            vec!["secrets_init", "secrets_unlock", "secrets_set", "secrets_get", "secrets_delete", "secrets_lock"]
        );
        for (_, details, tool) in &rows {
            assert_eq!(tool, "viewer");
            assert!(!details.contains("super-secret-value"), "значение в аудите: {details}");
        }
        let set_details = &rows[2].1;
        assert!(set_details.contains("\"size\":18"), "{set_details}");
        db.with_conn(|conn| {
            let actor: String =
                conn.query_row("SELECT actor FROM audit_log LIMIT 1", [], |r| r.get(0))?;
            assert_eq!(actor, "user");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn wrong_pin_writes_nothing() {
        let (tmp, db, mut ctl) = setup(1800);
        ctl.init("1234", "1234").unwrap();
        let before = audit_rows(&db).len();
        let err = ctl.unlock("9999").unwrap_err().to_string();
        assert!(err.contains("неверный PIN"), "{err}");
        assert_eq!(audit_rows(&db).len(), before, "неверный PIN не должен писать аудит");
        assert_eq!(ctl.state(&ro(&tmp)), SecretsState::Locked);
    }

    #[test]
    fn reinit_forbidden_with_hint() {
        let (tmp, _db, mut ctl) = setup(1800);
        ctl.init("1234", "1234").unwrap();
        let err = ctl.init("5678", "5678").unwrap_err().to_string();
        assert!(err.contains("уже инициализировано"), "{err}");
        assert!(err.contains("[Подсказка]"), "{err}");
        // Старый PIN по-прежнему работает.
        ctl.unlock("1234").unwrap();
        assert!(matches!(ctl.state(&ro(&tmp)), SecretsState::Unlocked { .. }));
    }

    #[test]
    fn init_validates_format_and_match() {
        let (_tmp, db, mut ctl) = setup(1800);
        assert!(ctl.init("12a4", "12a4").is_err());
        assert!(ctl.init("1234", "4321").is_err());
        assert!(audit_rows(&db).is_empty(), "ошибки формата не пишут аудит");
    }

    #[test]
    fn ttl_expiry_locks_session() {
        let (tmp, _db, mut ctl) = setup(0);
        ctl.init("1234", "1234").unwrap();
        ctl.unlock("1234").unwrap();
        // TTL=0: сессия протухает мгновенно, reveal гасит ключ.
        assert!(ctl.reveal("x").is_err());
        assert_eq!(ctl.state(&ro(&tmp)), SecretsState::Locked);
    }

    #[test]
    fn shutdown_zeroizes_and_is_idempotent() {
        let (tmp, _db, mut ctl) = setup(1800);
        ctl.init("1234", "1234").unwrap();
        ctl.unlock("1234").unwrap();
        ctl.shutdown();
        assert_eq!(ctl.state(&ro(&tmp)), SecretsState::Locked);
        ctl.shutdown(); // повторное закрытие — no-op
        assert!(ctl.reveal("x").is_err());
    }

    #[test]
    fn name_validation_via_service() {
        let (_tmp, _db, mut ctl) = setup(1800);
        ctl.init("1234", "1234").unwrap();
        ctl.unlock("1234").unwrap();
        assert!(ctl.set("bad name!", "v", None).is_err());
        assert!(ctl.set("", "v", None).is_err());
        assert!(ctl.set("good.name-1_x", "v", None).is_ok());
    }
}