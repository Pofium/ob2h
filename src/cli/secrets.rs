//! CLI команды управления хранилищем секретов `ob2h secrets` (Фаза 57).

use std::path::Path;
use std::sync::Arc;
use clap::Subcommand;
use crate::db::Database;
use crate::secrets::crypto::{derive_key, KdfParams};
use crate::secrets::ipc::IpcClient;
use crate::secrets::service::SecretsService;

#[derive(Subcommand, Debug)]
pub enum SecretsCommands {
    /// Инициализация нового сейфа секретов (запрашивает PIN)
    Init,
    /// Разблокировка сейфа мастер-ключом на основе PIN
    Unlock,
    /// Блокировка сейфа и затирание ключа из памяти
    Lock,
    /// Просмотр статуса хранилища секретов
    Status,
    /// Смена PIN-кода с перешифровкой всех записей
    Rekey,
    /// Список сохраненных секретов без значений
    List,
    /// Показать значение секрета по имени в stdout
    Show {
        /// Имя секрета
        name: String,
    },
}

pub struct SecretsCli;

impl SecretsCli {
    pub async fn handle(
        data_dir: &Path,
        db: Database,
        cmd: SecretsCommands,
        ttl_secs: u64,
    ) -> anyhow::Result<()> {
        let service = Arc::new(SecretsService::new(db.conn_arc(), ttl_secs));

        match cmd {
            SecretsCommands::Init => {
                if service.is_initialized()? {
                    println!("⚠️ Хранилище секретов уже инициализировано.");
                    return Ok(());
                }
                let pin = rpassword::prompt_password("Введите новый PIN (мин. 4 символа): ")?;
                let pin = pin.trim().to_string();
                if pin.len() < 4 {
                    eprintln!("❌ Ошибка: PIN-код должен содержать как минимум 4 символа.");
                    return Ok(());
                }
                let confirm = rpassword::prompt_password("Повторите новый PIN: ")?;
                if pin != confirm.trim() {
                    eprintln!("❌ Ошибка: введённые PIN-коды не совпадают.");
                    return Ok(());
                }

                service.init(&pin, None)?;
                println!("✓ Хранилище секретов успешно инициализировано (Argon2id + XChaCha20-Poly1305).");
            }

            SecretsCommands::Unlock => {
                if !service.is_initialized()? {
                    eprintln!("⚠️ Хранилище секретов не инициализировано: выполните 'ob2h secrets init'.");
                    return Ok(());
                }
                let pin = rpassword::prompt_password("Введите PIN: ")?;
                let pin = pin.trim().to_string();

                if let Err(e) = service.unlock(&pin) {
                    eprintln!("❌ {e}");
                    return Ok(());
                }

                let (salt_hex, kdf_json) = db.with_conn(|conn| {
                    let s: String = conn.query_row("SELECT value FROM kv WHERE key = 'secrets.salt'", [], |r| r.get(0))?;
                    let k: String = conn.query_row("SELECT value FROM kv WHERE key = 'secrets.kdf_params'", [], |r| r.get(0))?;
                    Ok((s, k))
                })?;
                let salt = hex::decode(salt_hex)?;
                let kdf_params: KdfParams = serde_json::from_str(&kdf_json)?;
                let key = derive_key(&pin, &salt, &kdf_params)?;

                let notified_server = IpcClient::send_action(data_dir, "unlock", Some(&*key)).await.unwrap_or(false);
                if notified_server {
                    println!("✓ Хранилище секретов успешно разблокировано (активный сервер ob2h serve уведомлен, TTL: {} мин).", ttl_secs / 60);
                } else {
                    println!("✓ PIN-код верен. (Сервер ob2h serve не запущен в фоновом режиме).");
                }
            }

            SecretsCommands::Lock => {
                let notified = IpcClient::send_action(data_dir, "lock", None).await.unwrap_or(false);
                if notified {
                    println!("✓ Хранилище секретов в активном сервере заблокировано, мастер-ключ затерт.");
                } else {
                    println!("✓ Хранилище секретов заблокировано.");
                }
            }

            SecretsCommands::Status => {
                let status = service.status()?;
                let is_running = IpcClient::read_server_info(data_dir).is_some();

                println!("Статус хранилища секретов:");
                println!("  • Инициализировано:  {}", if status.initialized { "да" } else { "нет (запустите 'ob2h secrets init')" });
                if status.initialized {
                    let state_str = if !status.locked {
                        format!("разблокировано (осталось ~{} сек)", status.ttl_left_sec.unwrap_or(0))
                    } else {
                        "заблокировано".to_string()
                    };
                    println!("  • Состояние CLI:     {}", state_str);
                    println!("  • Сервер serve:      {}", if is_running { "активен" } else { "не запущен" });
                    println!("  • Число записей:     {}", status.records_count);
                    if !status.names.is_empty() {
                        println!("  • Доступные имена:   {}", status.names.join(", "));
                    }
                }
            }

            SecretsCommands::Rekey => {
                if !service.is_initialized()? {
                    eprintln!("⚠️ Хранилище секретов не инициализировано: выполните 'ob2h secrets init'.");
                    return Ok(());
                }
                let old_pin = rpassword::prompt_password("Введите текущий PIN: ")?;
                let old_pin = old_pin.trim().to_string();
                if let Err(e) = service.unlock(&old_pin) {
                    eprintln!("❌ {e}");
                    return Ok(());
                }

                let new_pin = rpassword::prompt_password("Введите новый PIN (мин. 4 символа): ")?;
                let new_pin = new_pin.trim().to_string();
                if new_pin.len() < 4 {
                    eprintln!("❌ Ошибка: новый PIN-код должен содержать как минимум 4 символа.");
                    return Ok(());
                }
                let confirm = rpassword::prompt_password("Повторите новый PIN: ")?;
                if new_pin != confirm.trim() {
                    eprintln!("❌ Ошибка: введённые PIN-коды не совпадают.");
                    return Ok(());
                }

                service.rekey(&new_pin)?;

                let (salt_hex, kdf_json) = db.with_conn(|conn| {
                    let s: String = conn.query_row("SELECT value FROM kv WHERE key = 'secrets.salt'", [], |r| r.get(0))?;
                    let k: String = conn.query_row("SELECT value FROM kv WHERE key = 'secrets.kdf_params'", [], |r| r.get(0))?;
                    Ok((s, k))
                })?;
                let salt = hex::decode(salt_hex)?;
                let kdf_params: KdfParams = serde_json::from_str(&kdf_json)?;
                let key = derive_key(&new_pin, &salt, &kdf_params)?;
                let _ = IpcClient::send_action(data_dir, "unlock", Some(&*key)).await;

                println!("✓ PIN-код успешно изменен, все секреты перешифрованы.");
            }

            SecretsCommands::List => {
                if !service.is_initialized()? {
                    eprintln!("⚠️ Хранилище секретов не инициализировано.");
                    return Ok(());
                }
                let pin = rpassword::prompt_password("Введите PIN для просмотра списка: ")?;
                service.unlock(pin.trim())?;
                let list = service.list()?;
                if list.is_empty() {
                    println!("В хранилище нет сохраненных секретов.");
                } else {
                    println!("{:<24} {:<32} {:<20} {:<10}", "ИМЯ", "ПОДСКАЗКА", "СОЗДАН", "ОБРАЩЕНИЙ");
                    println!("{:-<24} {:-<32} {:-<20} {:-<10}", "", "", "", "");
                    for item in list {
                        println!(
                            "{:<24} {:<32} {:<20} {:<10}",
                            item.name,
                            item.hint.unwrap_or_else(|| "-".to_string()),
                            &item.created_at[..19.min(item.created_at.len())],
                            item.access_count
                        );
                    }
                }
            }

            SecretsCommands::Show { name } => {
                if !service.is_initialized()? {
                    eprintln!("⚠️ Хранилище секретов не инициализировано.");
                    return Ok(());
                }
                let pin = rpassword::prompt_password("Введите PIN для расшифровки: ")?;
                service.unlock(pin.trim())?;
                match service.get(&name) {
                    Ok(val) => {
                        println!("{}", val.as_str());
                    }
                    Err(e) => {
                        eprintln!("❌ {e}");
                    }
                }
            }
        }

        Ok(())
    }
}
