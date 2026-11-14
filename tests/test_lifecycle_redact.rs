//! Тесты скрабинга секретов (v1.9.9, Ф69.1): матрица паттернов, имена Secrets
//! Vault, гейт OB2H_REDACT, интеграция с session_log (без утечки в daily-лог).

use std::sync::{Mutex, OnceLock};

use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::lifecycle::redact::{enabled, redact_for_capture, redact_with};
use tempfile::tempdir;

/// Env-гейт — глобальное состояние процесса: тесты, которые его трогают или
/// зовут capture-путь, сериализуются этим локом (иначе гонки между параллельными
/// тестами одного бинарника).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn no_names() -> Vec<String> {
    Vec::new()
}

#[test]
fn test_redact_patterns_matrix() {
    let names = no_names();

    // OpenAI-стиль: sk- + длинный хвост
    let out = redact_with(&names, "ключ sk-prod1234567890abcdef в логе");
    assert!(out.contains("[REDACTED:sk-key]"), "{out}");
    assert!(!out.contains("sk-prod1234567890abcdef"));

    // GitHub-токены
    let out = redact_with(&names, "токен ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ123 в тексте");
    assert!(out.contains("[REDACTED:gh-token]"), "{out}");

    // Bearer — двух-токенный паттерн
    let out = redact_with(&names, "Authorization: Bearer abc.def.ghi-jkl-mno-pqr");
    assert!(out.contains("[REDACTED:bearer]"), "{out}");
    assert!(!out.contains("abc.def.ghi-jkl-mno-pqr"));

    // URL-userinfo
    let out = redact_with(&names, "подключение к postgres://admin:pa55w0rd@db.host:5432/base");
    assert!(out.contains("postgres://[REDACTED]@db.host:5432/base"), "{out}");

    // OB2H_ и generic key=value
    let out = redact_with(&names, "OB2H_LLM_API_KEY=abc123 и API_TOKEN=tok456");
    assert!(out.contains("OB2H_LLM_API_KEY=[REDACTED]"), "{out}");
    assert!(out.contains("API_TOKEN=[REDACTED]"), "{out}");

    // Обычный текст не трогается
    let text = "обычный текст про архитектуру ob2h без секретов";
    assert_eq!(redact_with(&names, text), text);

    // Разделители сохраняются байт-в-байт (новые строки, двойные пробелы)
    let text = "строка 1\n\nстрока   2  с ключом sk-prod1234567890abcdef\n";
    let out = redact_with(&names, text);
    assert!(out.contains("строка 1\n\nстрока   2  с ключом [REDACTED:sk-key]\n"), "{out}");

    // Короткие/ложные кандидаты не маскируются
    assert_eq!(redact_with(&names, "ск-json и sk-1 обычные слова"), "ск-json и sk-1 обычные слова");
}

#[test]
fn test_redact_vault_names() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();
    let ctx = init_app(settings)?;

    ctx.secrets.init("1234", None)?;
    ctx.secrets.unlock("1234")?;
    ctx.secrets.set("api.key", "настоящее-значение-секрета", Some("тест"))?;

    let out = redact_for_capture(&ctx.db, "конфиг api.key=xyz123 прочитан");
    assert!(out.contains("api.key=[REDACTED:vault]"), "{out}");
    assert!(!out.contains("xyz123"));

    // Голое имя без присваивания не маскируется (читаемость важнее)
    let out = redact_for_capture(&ctx.db, "имя записи api.key упоминается в тексте");
    assert!(out.contains("api.key упоминается"), "{out}");
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
// Лок держится через await осознанно: он защищает env-гейт OB2H_REDACT от
// параллельных тестов процесса на протяжении capture-вызова (однопоточный
// runtime tokio::test — гонок внутри теста нет).
async fn test_session_capture_redacts_into_daily() -> anyhow::Result<()> {
    let _guard = env_lock();
    let tmp = tempdir()?;
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();
    let ctx = init_app(settings)?;
    let server = ob2h::mcp::McpServer::new(ctx.clone());

    let out = server
        .call_tool(
            "session_log",
            serde_json::json!({
                "user_text": "ключ sk-prod1234567890abcdef утёк в лог",
                "assistant_text": "принял"
            }),
        )
        .await;
    assert!(out.starts_with("logged"), "{out}");

    // В daily-логе — маска, сырца нет
    let daily = ctx.settings.workspace_dir().join("daily");
    let files: Vec<_> = std::fs::read_dir(&daily)?.flatten().collect();
    assert!(!files.is_empty());
    for f in files {
        let text = std::fs::read_to_string(f.path())?;
        assert!(!text.contains("sk-prod1234567890abcdef"), "утечка: {text}");
        if text.contains("sk-") {
            assert!(text.contains("[REDACTED:sk-key]"), "{text}");
        }
    }
    Ok(())
}

#[test]
fn test_redact_gate_env() {
    let _guard = env_lock();
    std::env::set_var("OB2H_REDACT", "0");
    assert!(!enabled());
    std::env::set_var("OB2H_REDACT", "1");
    assert!(enabled());
    std::env::remove_var("OB2H_REDACT");
    assert!(enabled(), "дефолт — включён");
}

/// Страховка от расползания env-гейта: смежный тест гарантирует дефолт после
/// снятия переменной (в этом бинарнике env трогает только test_redact_gate_env
/// под тем же локом, так что гонок нет).
#[test]
fn test_redact_capture_uses_gate() {
    let _guard = env_lock();
    let tmp = tempdir().unwrap();
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.embed_provider = "fake".to_string();
    let ctx = init_app(settings).unwrap();

    std::env::set_var("OB2H_REDACT", "0");
    let out = redact_for_capture(&ctx.db, "ключ sk-prod1234567890abcdef");
    std::env::remove_var("OB2H_REDACT");
    assert!(out.contains("sk-prod1234567890abcdef"), "гейт=0 должен пропускать текст: {out}");
}
