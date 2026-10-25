//! Тесты крипто-ядра и сервиса SecretsService (Фаза 56).

use ob2h::db::Database;
use ob2h::secrets::crypto::*;
use ob2h::secrets::service::SecretsService;

#[test]
fn test_crypto_roundtrip_and_errors() {
    let salt = generate_salt().expect("salt");
    let params = KdfParams {
        m_cost: 1024,
        t_cost: 1,
        p_cost: 1,
    };
    let key = derive_key("master_pin_9876", &salt, &params).expect("derive");

    let val = "SECRET_PROD_DATABASE_PASSWORD_$$12345";
    let ct = encrypt_value(&key, "db_pass", val.as_bytes()).expect("encrypt");

    // 1. Успешная расшифровка
    let decrypted = decrypt_value(&key, "db_pass", &ct).expect("decrypt");
    assert_eq!(String::from_utf8(decrypted.to_vec()).unwrap(), val);

    // 2. Неверный aad (имя секрета)
    let bad_aad = decrypt_value(&key, "wrong_name", &ct);
    assert!(bad_aad.is_err());

    // 3. Неверный ключ
    let wrong_key = derive_key("wrong_pin", &salt, &params).expect("derive");
    let bad_key = decrypt_value(&wrong_key, "db_pass", &ct);
    assert!(bad_key.is_err());

    // 4. Повреждение 1 байта
    let mut corrupted = ct.clone();
    corrupted[28] ^= 0x01;
    let bad_ct = decrypt_value(&key, "db_pass", &corrupted);
    assert!(bad_ct.is_err());
}

#[test]
fn test_secrets_migration_m9_idempotence() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    // Проверяем версию
    let v: String = db.with_conn(|conn| {
        conn.query_row("SELECT value FROM kv WHERE key = 'schema_version'", [], |r| r.get(0))
    })?;
    assert_eq!(v, "9");

    // Проверяем наличие таблицы secrets
    let count: i64 = db.with_conn(|conn| {
        conn.query_row("SELECT COUNT(*) FROM secrets", [], |r| r.get(0))
    })?;
    assert_eq!(count, 0);

    // Повторная миграция не должна падать (идемпотентность)
    db.with_conn(|conn| ob2h::db::schema::migrate(conn))?;

    Ok(())
}

#[test]
fn test_secrets_service_lifecycle() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    let service = SecretsService::new(db.conn_arc(), 300);

    // 1. До init
    assert!(!service.is_initialized()?);
    let status_before = service.status()?;
    assert!(!status_before.initialized);
    assert!(status_before.locked);
    assert!(service.get("any").is_err());

    // 2. Init
    service.init("user_pin_4321", Some(KdfParams { m_cost: 1024, t_cost: 1, p_cost: 1 }))?;
    assert!(service.is_initialized()?);

    // Повторный init запрещен
    assert!(service.init("another", None).is_err());

    // 3. Unlock неверным PIN
    assert!(service.unlock("wrong").is_err());
    let status_locked = service.status()?;
    assert!(status_locked.locked);

    // 4. Unlock верным PIN
    service.unlock("user_pin_4321")?;
    let status_unlocked = service.status()?;
    assert!(!status_unlocked.locked);
    assert!(status_unlocked.ttl_left_sec.unwrap() > 0);

    // 5. Set & Get
    service.set("api_key", "sk-1234567890abcdef", Some("OpenAI prod key"))?;
    let secret = service.get("api_key")?;
    assert_eq!(secret.as_str(), "sk-1234567890abcdef");

    // 6. List
    let list = service.list()?;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "api_key");
    assert_eq!(list[0].hint.as_deref(), Some("OpenAI prod key"));

    // 7. Rekey
    service.rekey("new_strong_pin_7777")?;
    // После rekey значение по-прежнему доступно в текущей сессии
    let secret_after = service.get("api_key")?;
    assert_eq!(secret_after.as_str(), "sk-1234567890abcdef");

    // 8. Lock & Unlock с новым PIN
    service.lock();
    assert!(service.status()?.locked);
    assert!(service.get("api_key").is_err());

    // Старый PIN не работает
    assert!(service.unlock("user_pin_4321").is_err());

    // Новый PIN работает
    service.unlock("new_strong_pin_7777")?;
    assert_eq!(service.get("api_key")?.as_str(), "sk-1234567890abcdef");

    // 9. Delete
    let deleted = service.delete("api_key")?;
    assert!(deleted);
    assert_eq!(service.list()?.len(), 0);

    Ok(())
}
