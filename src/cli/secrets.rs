//! CLI команды управления хранилищем секретов `ob2h secrets` (Фаза 57).

use std::path::Path;
use std::sync::Arc;
use clap::Subcommand;
use crate::db::Database;
use crate::secrets::crypto::{derive_key, KdfParams};
use crate::secrets::ipc::IpcClient;
use crate::secrets::pin_input::{is_valid_pin, PinPurpose, NEW_PIN_FORMAT_ERROR, PIN_FORMAT_ERROR};
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
    /// Скрытый хелпер PIN-диалога для MCP (issue #1): окно в отдельном процессе,
    /// результат — JSON в stdout. Не для ручного вызова.
    #[command(hide = true)]
    AskPin {
        /// Назначение ввода: unlock | new | confirm
        #[arg(long, default_value = "unlock")]
        purpose: String,
    },
}

/// Итог попытки графического ввода PIN.
///
/// Без feature `gui` окно недоступно, поэтому варианты `Pin`/`Cancelled`
/// не конструируются — это ожидаемо.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
enum GuiPrompt {
    /// Окно недоступно (нет GUI, headless, `OB2H_PIN_GUI=0`) — нужен терминал.
    Unavailable,
    /// Пользователь ввёл PIN (ровно 4 цифры).
    Pin(String),
    /// Пользователь отменил ввод.
    Cancelled,
}

/// Контрактовая ошибка при неинтерактивном stdin (issue #2.1): rpassword без
/// терминала висит вечно — автоматизация должна получать отказ сразу.
const PIN_NO_TTY_ERROR: &str = "[Error] PIN-запрос требует интерактивный терминал или GUI-сборку [Подсказка: OB2H_PIN_GUI=1 + билд с --features gui, либо выполните unlock вручную]";

/// Чистая логика TTY-гейта (вынесена для тестов): `Ok(())` — терминальный ввод
/// разрешён; `Err(msg)` — контрактовая ошибка для неинтерактивного stdin.
fn ensure_terminal_input(is_tty: bool) -> Result<(), String> {
    if is_tty {
        Ok(())
    } else {
        Err(PIN_NO_TTY_ERROR.to_string())
    }
}

/// Запрашивает одиночный PIN через локальное окно (feature `gui`).
///
/// Вызывается до первого `.await` команды: окну eframe нужен главный поток процесса.
fn gui_prompt(purpose: PinPurpose) -> GuiPrompt {
    #[cfg(feature = "gui")]
    {
        if !crate::secrets::pin_input::gui_disabled_by_env() {
            match crate::secrets::pin_gui::ask_pin(purpose) {
                Ok(Some(pin)) => return GuiPrompt::Pin(pin),
                Ok(None) => return GuiPrompt::Cancelled,
                Err(err) => {
                    eprintln!("⚠️ Окно ввода PIN недоступно ({err}); ввод в терминале.");
                    return GuiPrompt::Unavailable;
                }
            }
        }
    }
    #[cfg(not(feature = "gui"))]
    let _ = purpose;
    GuiPrompt::Unavailable
}

/// Запрашивает новый PIN с подтверждением через локальное окно (feature `gui`).
fn gui_prompt_new() -> GuiPrompt {
    #[cfg(feature = "gui")]
    {
        if !crate::secrets::pin_input::gui_disabled_by_env() {
            match crate::secrets::pin_gui::ask_pin_twice(PinPurpose::New, PinPurpose::Confirm) {
                Ok(Some(pin)) => return GuiPrompt::Pin(pin),
                Ok(None) => return GuiPrompt::Cancelled,
                Err(err) => {
                    eprintln!("⚠️ Окно ввода PIN недоступно ({err}); ввод в терминале.");
                    return GuiPrompt::Unavailable;
                }
            }
        }
    }
    GuiPrompt::Unavailable
}

/// Возвращает PIN: сначала окно GUI, иначе — терминал.
///
/// `None` означает отмену ввода (окно закрыто/Esc).
fn resolve_pin(purpose: PinPurpose, terminal_prompt: &str) -> anyhow::Result<Option<String>> {
    match gui_prompt(purpose) {
        GuiPrompt::Pin(pin) => Ok(Some(pin)),
        GuiPrompt::Cancelled => {
            eprintln!("⚠️ Ввод PIN-кода отменён.");
            Ok(None)
        }
        GuiPrompt::Unavailable => {
            ensure_terminal_input(std::io::IsTerminal::is_terminal(&std::io::stdin()))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let pin = rpassword::prompt_password(terminal_prompt)?.trim().to_string();
            Ok(Some(pin))
        }
    }
}

/// Возвращает новый PIN с подтверждением: окно GUI, иначе — терминал.
///
/// `None` — отмена либо ошибка валидации (сообщение уже выведено).
fn resolve_new_pin(invalid_msg: &str, mismatch_msg: &str) -> anyhow::Result<Option<String>> {
    match gui_prompt_new() {
        GuiPrompt::Pin(pin) => Ok(Some(pin)),
        GuiPrompt::Cancelled => {
            eprintln!("⚠️ Ввод PIN-кода отменён.");
            Ok(None)
        }
        GuiPrompt::Unavailable => {
            ensure_terminal_input(std::io::IsTerminal::is_terminal(&std::io::stdin()))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let pin = rpassword::prompt_password("Введите новый PIN (ровно 4 цифры): ")?
                .trim()
                .to_string();
            if !is_valid_pin(&pin) {
                eprintln!("{invalid_msg}");
                return Ok(None);
            }
            let confirm = rpassword::prompt_password("Повторите новый PIN: ")?
                .trim()
                .to_string();
            if pin != confirm {
                eprintln!("{mismatch_msg}");
                return Ok(None);
            }
            Ok(Some(pin))
        }
    }
}

/// Парсинг `--purpose` хелпера ask-pin (issue #1). Неизвестное — unlock (дефолт).
pub fn parse_purpose(raw: &str) -> PinPurpose {
    match raw.trim().to_ascii_lowercase().as_str() {
        "new" => PinPurpose::New,
        "confirm" => PinPurpose::Confirm,
        "current" => PinPurpose::Current,
        _ => PinPurpose::Unlock,
    }
}

/// Итог окна ask-pin → (JSON stdout, код выхода).
///
/// Гейт окна — как в CLI: gui-сборка и `OB2H_PIN_GUI!=0`; иначе «недоступно».
/// PIN уходит только в stdout родительского процесса ob2h — не в логи/аудит
/// (ADR-18/ADR-21).
fn ask_pin_outcome_json(purpose: PinPurpose) -> (String, i32) {
    #[cfg(feature = "gui")]
    {
        if !crate::secrets::pin_input::gui_disabled_by_env() {
            return match crate::secrets::pin_gui::ask_pin(purpose) {
                Ok(Some(pin)) => (
                    serde_json::json!({ "outcome": "pin", "pin": pin }).to_string(),
                    0,
                ),
                Ok(None) => (r#"{"outcome":"cancelled"}"#.to_string(), 0),
                Err(_) => (r#"{"outcome":"unavailable"}"#.to_string(), 1),
            };
        }
    }
    #[cfg(not(feature = "gui"))]
    let _ = purpose;
    (r#"{"outcome":"unavailable"}"#.to_string(), 1)
}

pub struct SecretsCli;

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #2.1: неинтерактивный stdin — контрактовая ошибка, не зависание.
    #[test]
    fn no_tty_gives_contract_error() {
        let err = ensure_terminal_input(false).expect_err("non-tty должен отклоняться");
        assert!(err.starts_with("[Error] PIN-запрос требует интерактивный терминал"));
        assert!(err.contains("[Подсказка:"));
        // Подсказка упоминает оба выхода: GUI-сборку и ручной CLI unlock.
        assert!(err.contains("--features gui"));
        assert!(err.contains("unlock"));
    }

    #[test]
    fn tty_gate_passes_for_terminal() {
        assert!(ensure_terminal_input(true).is_ok());
    }

    /// Issue #1: парсинг --purpose хелпера ask-pin.
    #[test]
    fn ask_pin_purpose_parsing() {
        assert_eq!(parse_purpose("unlock"), PinPurpose::Unlock);
        assert_eq!(parse_purpose("NEW"), PinPurpose::New);
        assert_eq!(parse_purpose(" confirm "), PinPurpose::Confirm);
        // Неизвестное и пустое — дефолт unlock.
        assert_eq!(parse_purpose("что-то"), PinPurpose::Unlock);
        assert_eq!(parse_purpose(""), PinPurpose::Unlock);
    }

    /// Issue #1: JSON-протокол хелпера — в не-gui окружении недоступно (exit 1).
    #[test]
    fn ask_pin_outcome_json_unavailable_without_gui() {
        let (json, code) = ask_pin_outcome_json(PinPurpose::Unlock);
        assert_eq!(json, r#"{"outcome":"unavailable"}"#);
        assert_eq!(code, 1);
    }
}

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
                let Some(pin) = resolve_new_pin(
                    &format!("❌ Ошибка: {PIN_FORMAT_ERROR}."),
                    "❌ Ошибка: введённые PIN-коды не совпадают.",
                )? else {
                    return Ok(());
                };

                service.init(&pin, None)?;
                println!("✓ Хранилище секретов успешно инициализировано (Argon2id + XChaCha20-Poly1305).");
            }

            SecretsCommands::Unlock => {
                if !service.is_initialized()? {
                    eprintln!("⚠️ Хранилище секретов не инициализировано: выполните 'ob2h secrets init'.");
                    return Ok(());
                }
                let Some(pin) = resolve_pin(PinPurpose::Unlock, "Введите PIN: ")? else {
                    return Ok(());
                };

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
                let Some(old_pin) = resolve_pin(PinPurpose::Current, "Введите текущий PIN: ")?
                else {
                    return Ok(());
                };
                if let Err(e) = service.unlock(&old_pin) {
                    eprintln!("❌ {e}");
                    return Ok(());
                }

                let Some(new_pin) = resolve_new_pin(
                    &format!("❌ Ошибка: {NEW_PIN_FORMAT_ERROR}."),
                    "❌ Ошибка: введённые PIN-коды не совпадают.",
                )? else {
                    return Ok(());
                };

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
                let Some(pin) = resolve_pin(PinPurpose::Unlock, "Введите PIN для просмотра списка: ")?
                else {
                    return Ok(());
                };
                service.unlock(&pin)?;
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
                let Some(pin) = resolve_pin(PinPurpose::Unlock, "Введите PIN для расшифровки: ")?
                else {
                    return Ok(());
                };
                service.unlock(&pin)?;
                match service.get(&name) {
                    Ok(val) => {
                        println!("{}", val.as_str());
                    }
                    Err(e) => {
                        eprintln!("❌ {e}");
                    }
                }
            }

            SecretsCommands::AskPin { purpose } => {
                // Issue #1: окно eframe — главный поток ЭТОГО процесса; результат —
                // JSON в stdout родителя. Родитель (MCP-сервер) разблокирует себя сам.
                let purpose = parse_purpose(&purpose);
                let (json, code) = ask_pin_outcome_json(purpose);
                println!("{json}");
                if code != 0 {
                    std::process::exit(code);
                }
            }
        }

        Ok(())
    }
}
