//! Сервис управления зашифрованными секретами (SecretsService).
//! Мастер-ключ K хранится исключительно в памяти процесса (Zeroizing)
//! и автоматически затирается при блокировке или по истечении TTL.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::info;
use zeroize::Zeroizing;

use super::crypto::{
    check_verifier, create_verifier, decrypt_value, derive_key, encrypt_value, generate_salt,
    KdfParams,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretsStatus {
    pub initialized: bool,
    pub locked: bool,
    pub ttl_left_sec: Option<u64>,
    pub records_count: usize,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretRecordMeta {
    pub name: String,
    pub hint: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_accessed: Option<String>,
    pub access_count: i64,
}

struct UnlockedSession {
    key: Zeroizing<[u8; 32]>,
    deadline: Instant,
    ttl: Duration,
}

pub struct SecretsService {
    conn: Arc<Mutex<Connection>>,
    session: Mutex<Option<UnlockedSession>>,
    default_ttl: Duration,
}

impl SecretsService {
    pub fn new(conn: Arc<Mutex<Connection>>, default_ttl_secs: u64) -> Self {
        Self {
            conn,
            session: Mutex::new(None),
            default_ttl: Duration::from_secs(default_ttl_secs),
        }
    }

    /// Проверка, инициализирован ли сейф секретов в БД.
    pub fn is_initialized(&self) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let val: Option<String> = conn
            .query_row(
                "SELECT value FROM kv WHERE key = 'secrets.initialized'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(val.as_deref() == Some("1"))
    }

    /// Инициализация нового сейфа (CLI `ob2h secrets init`).
    pub fn init(&self, pin: &str, params: Option<KdfParams>) -> anyhow::Result<()> {
        if pin.len() < 4 {
            anyhow::bail!("[Error] PIN-код должен содержать как минимум 4 символа");
        }
        if self.is_initialized()? {
            anyhow::bail!("[Error] хранилище секретов уже инициализировано");
        }

        let kdf_params = params.unwrap_or_default();
        let salt = generate_salt()?;
        let key = derive_key(pin, &salt, &kdf_params)?;
        let verifier = create_verifier(&key)?;

        let kdf_json = serde_json::to_string(&kdf_params)?;
        let salt_hex = hex::encode(salt);
        let verifier_hex = hex::encode(verifier);

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('secrets.initialized', '1')",
            [],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('secrets.salt', ?1)",
            params![salt_hex],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('secrets.kdf_params', ?1)",
            params![kdf_json],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('secrets.verifier', ?1)",
            params![verifier_hex],
        )?;
        tx.commit()?;

        info!("Хранилище секретов успешно инициализировано");
        Ok(())
    }

    /// Разблокировка хранилища мастер-ключом на основе введённого PIN (CLI `ob2h secrets unlock`).
    pub fn unlock(&self, pin: &str) -> anyhow::Result<()> {
        if !self.is_initialized()? {
            anyhow::bail!("[Error] хранилище секретов не инициализировано: выполните 'ob2h secrets init'");
        }

        let (salt_hex, kdf_json, verifier_hex): (String, String, String) = {
            let conn = self.conn.lock().unwrap();
            let salt: String = conn.query_row(
                "SELECT value FROM kv WHERE key = 'secrets.salt'",
                [],
                |r| r.get(0),
            )?;
            let kdf: String = conn.query_row(
                "SELECT value FROM kv WHERE key = 'secrets.kdf_params'",
                [],
                |r| r.get(0),
            )?;
            let verifier: String = conn.query_row(
                "SELECT value FROM kv WHERE key = 'secrets.verifier'",
                [],
                |r| r.get(0),
            )?;
            (salt, kdf, verifier)
        };

        let salt = hex::decode(salt_hex).map_err(|e| anyhow::anyhow!("Битый salt hex: {e}"))?;
        let kdf_params: KdfParams =
            serde_json::from_str(&kdf_json).map_err(|e| anyhow::anyhow!("Битый kdf_params: {e}"))?;
        let verifier =
            hex::decode(verifier_hex).map_err(|e| anyhow::anyhow!("Битый verifier hex: {e}"))?;

        let key = derive_key(pin, &salt, &kdf_params)?;
        if !check_verifier(&key, &verifier) {
            anyhow::bail!("[Error] неверный PIN-код");
        }

        let deadline = Instant::now() + self.default_ttl;
        let mut session_guard = self.session.lock().unwrap();
        *session_guard = Some(UnlockedSession {
            key,
            deadline,
            ttl: self.default_ttl,
        });

        info!("Хранилище секретов разблокировано (TTL: {} сек)", self.default_ttl.as_secs());
        Ok(())
    }

    /// Блокировка хранилища и немедленное затирание ключа из памяти.
    pub fn lock(&self) {
        let mut session_guard = self.session.lock().unwrap();
        *session_guard = None;
        info!("Хранилище секретов заблокировано");
    }

    /// Получение активного ключа шифрования с проверкой TTL (продлевает TTL при успехе).
    fn get_active_key(&self) -> anyhow::Result<Zeroizing<[u8; 32]>> {
        if !self.is_initialized()? {
            anyhow::bail!("[Error] secrets not initialized: запустите \"ob2h secrets init\"");
        }
        let mut guard = self.session.lock().unwrap();
        if let Some(ref mut session) = *guard {
            let now = Instant::now();
            if now > session.deadline {
                *guard = None;
                anyhow::bail!("[Error] secrets locked: срок действия сессии истек");
            }
            // Продлеваем активность
            session.deadline = now + session.ttl;
            Ok(session.key.clone())
        } else {
            anyhow::bail!("[Error] secrets locked: запустите \"ob2h secrets unlock\"");
        }
    }

    /// Статус хранилища (доступен без разблокировки).
    pub fn status(&self) -> anyhow::Result<SecretsStatus> {
        let initialized = self.is_initialized()?;
        if !initialized {
            return Ok(SecretsStatus {
                initialized: false,
                locked: true,
                ttl_left_sec: None,
                records_count: 0,
                names: Vec::new(),
            });
        }

        let (locked, ttl_left) = {
            let mut guard = self.session.lock().unwrap();
            if let Some(ref session) = *guard {
                let now = Instant::now();
                if now > session.deadline {
                    *guard = None;
                    (true, None)
                } else {
                    let left = session.deadline.duration_since(now).as_secs();
                    (false, Some(left))
                }
            } else {
                (true, None)
            }
        };

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT name FROM secrets ORDER BY name ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut names = Vec::new();
        for r in rows {
            names.push(r?);
        }
        let records_count = names.len();

        Ok(SecretsStatus {
            initialized: true,
            locked,
            ttl_left_sec: ttl_left,
            records_count,
            names,
        })
    }

    /// Валидация имени секрета [A-Za-z0-9._-]{1,128}
    fn validate_name(name: &str) -> anyhow::Result<()> {
        if name.is_empty() || name.len() > 128 {
            anyhow::bail!("[Error] имя секрета должно содержать от 1 до 128 символов");
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            anyhow::bail!("[Error] недопустимые символы в имени секрета (разрешены [A-Za-z0-9._-])");
        }
        Ok(())
    }

    /// Сохранение/обновление зашифрованного секрета (только Unlocked).
    pub fn set(&self, name: &str, value: &str, hint: Option<&str>) -> anyhow::Result<()> {
        Self::validate_name(name)?;
        if value.len() > 65536 {
            anyhow::bail!("[Error] значение секрета превышает максимальный размер (64 КБ)");
        }
        let key = self.get_active_key()?;
        let ciphertext = encrypt_value(&key, name, value.as_bytes())?;
        let now = Utc::now().to_rfc3339();

        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"INSERT INTO secrets (name, ciphertext, hint, created_at, updated_at, access_count)
               VALUES (?1, ?2, ?3, ?4, ?4, 0)
               ON CONFLICT(name) DO UPDATE SET
                   ciphertext = excluded.ciphertext,
                   hint = coalesce(excluded.hint, secrets.hint),
                   updated_at = excluded.updated_at"#,
            params![name, ciphertext, hint, now],
        )?;

        info!("secrets: set \"{}\" (ok)", name);
        Ok(())
    }

    /// Расшифровка и получение значения секрета (только Unlocked).
    pub fn get(&self, name: &str) -> anyhow::Result<Zeroizing<String>> {
        Self::validate_name(name)?;
        let key = self.get_active_key()?;

        let (ciphertext, hint): (Vec<u8>, Option<String>) = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT ciphertext, hint FROM secrets WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("[Error] секрет \"{}\" не найден", name))?
        };

        let decrypted_bytes = decrypt_value(&key, name, &ciphertext)?;
        let value = String::from_utf8(decrypted_bytes.to_vec())
            .map_err(|_| anyhow::anyhow!("[Error] расшифрованное значение не является валидным UTF-8"))?;

        // Обновляем статистику обращений
        let now = Utc::now().to_rfc3339();
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "UPDATE secrets SET last_accessed = ?1, access_count = access_count + 1 WHERE name = ?2",
                params![now, name],
            );
        }

        info!("secrets: get \"{}\" (ok, hint={:?})", name, hint);
        Ok(Zeroizing::new(value))
    }

    /// Удаление секрета (только Unlocked).
    pub fn delete(&self, name: &str) -> anyhow::Result<bool> {
        Self::validate_name(name)?;
        let _key = self.get_active_key()?;

        let conn = self.conn.lock().unwrap();
        let affected = conn.execute("DELETE FROM secrets WHERE name = ?1", params![name])?;
        info!("secrets: delete \"{}\" (affected={})", name, affected);
        Ok(affected > 0)
    }

    /// Список метаданных секретов без значений (только Unlocked).
    pub fn list(&self) -> anyhow::Result<Vec<SecretRecordMeta>> {
        let _key = self.get_active_key()?;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, hint, created_at, updated_at, last_accessed, access_count FROM secrets ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SecretRecordMeta {
                name: row.get(0)?,
                hint: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                last_accessed: row.get(4)?,
                access_count: row.get(5)?,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Смена PIN-кода с перешифровкой всех существующих записей (только Unlocked).
    pub fn rekey(&self, new_pin: &str) -> anyhow::Result<()> {
        if new_pin.len() < 4 {
            anyhow::bail!("[Error] новый PIN-код должен содержать как минимум 4 символа");
        }
        let old_key = self.get_active_key()?;

        // 1. Читаем все существующие секреты и расшифровываем старым ключом в памяти
        let records: Vec<(String, Option<String>, Zeroizing<Vec<u8>>, String, String, Option<String>, i64)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT name, hint, ciphertext, created_at, updated_at, last_accessed, access_count FROM secrets",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?;
            let mut list = Vec::new();
            for r in rows {
                let (name, hint, ct, cr, up, la, ac) = r?;
                let pt = decrypt_value(&old_key, &name, &ct)?;
                list.push((name, hint, pt, cr, up, la, ac));
            }
            list
        };

        // 2. Генерируем новую соль и ключ K'
        let new_salt = generate_salt()?;
        let kdf_params = KdfParams::default();
        let new_key = derive_key(new_pin, &new_salt, &kdf_params)?;
        let new_verifier = create_verifier(&new_key)?;

        // 3. Зашифровываем все записи новым ключом в памяти
        let mut reencrypted = Vec::with_capacity(records.len());
        for (name, hint, pt, cr, up, la, ac) in &records {
            let new_ct = encrypt_value(&new_key, name, pt.as_slice())?;
            reencrypted.push((name.clone(), hint.clone(), new_ct, cr.clone(), up.clone(), la.clone(), *ac));
        }

        // 4. Атомарная транзакция записи в БД
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        tx.execute(
            "UPDATE kv SET value = ?1 WHERE key = 'secrets.salt'",
            params![hex::encode(new_salt)],
        )?;
        tx.execute(
            "UPDATE kv SET value = ?1 WHERE key = 'secrets.kdf_params'",
            params![serde_json::to_string(&kdf_params)?],
        )?;
        tx.execute(
            "UPDATE kv SET value = ?1 WHERE key = 'secrets.verifier'",
            params![hex::encode(new_verifier)],
        )?;

        tx.execute("DELETE FROM secrets", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO secrets (name, hint, ciphertext, created_at, updated_at, last_accessed, access_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (name, hint, ct, cr, up, la, ac) in reencrypted {
                stmt.execute(params![name, hint, ct, cr, up, la, ac])?;
            }
        }
        tx.commit()?;

        // 5. Обновляем сессию на новый ключ
        let mut guard = self.session.lock().unwrap();
        if let Some(ref mut session) = *guard {
            session.key = new_key;
            session.deadline = Instant::now() + session.ttl;
        }

        info!("Смена PIN-кода и перешифровка {} записей завершена успешно", records.len());
        Ok(())
    }
}
