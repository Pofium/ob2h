//! Скрабинг секретов в конвейерах захвата (v1.9.9, Ф69.1): перед записью в
//! daily-логи (session_log/session_ingest — а значит, и во все LLM-конвейеры,
//! которые читают логи: дрим/консолидатор/рекап) текст маскируется по паттернам
//! ключей и по именам записей Secrets Vault (имя известно без unlock; значения
//! зашифрованы и не могут утечь текстом).
//! Замена — `[REDACTED:…]`; гейт `OB2H_REDACT=0` отключает скрабинг.
//! Реализация без regex-крейта (ADR-стиль репо: ручные сканеры, как в ast.rs).

use crate::db::Database;

/// Env-гейт скрабинга (по умолчанию включён).
pub const REDACT_ENV: &str = "OB2H_REDACT";

pub fn enabled() -> bool {
    match std::env::var(REDACT_ENV) {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")),
        Err(_) => true,
    }
}

/// Имена записей Secrets Vault (без unlock — таблица хранит только шифротекст).
fn vault_names(db: &Database) -> Vec<String> {
    db.with_conn(|conn| {
        let mut stmt = match conn.prepare("SELECT name FROM secrets") {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    })
    .unwrap_or_default()
}

/// Точка врезки для захвата: гейт + имена vault + маскирование.
pub fn redact_for_capture(db: &Database, text: &str) -> String {
    if !enabled() || text.is_empty() {
        return text.to_string();
    }
    let names = vault_names(db);
    redact_with(&names, text)
}

fn mask_value(token: &str, sep_idx: usize, marker: &str) -> String {
    let prefix: String = token[..=sep_idx].to_string();
    format!("{prefix}[{marker}]")
}

/// Маскирование одного whitespace-токена.
fn mask_token(names: &[String], token: &str) -> String {
    // 1. OpenAI-стиль: sk-… (длинный хвост)
    if let Some(rest) = token.strip_prefix("sk-") {
        if rest.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').count() >= 16 {
            return "[REDACTED:sk-key]".to_string();
        }
    }
    // 2. GitHub-токены: ghp_/gho_/ghs_/ghu_…
    for p in ["ghp_", "gho_", "ghs_", "ghu_"] {
        if let Some(rest) = token.strip_prefix(p) {
            if rest.chars().filter(|c| c.is_alphanumeric()).count() >= 20 {
                return "[REDACTED:gh-token]".to_string();
            }
        }
    }
    // 3. URL-userinfo: scheme://user:pass@host
    if let Some(pos) = token.find("://") {
        let after = &token[pos + 3..];
        let slash = after.find('/').unwrap_or(after.len());
        let userinfo = &after[..slash.min(after.len())];
        if let Some(at) = userinfo.find('@') {
            if userinfo[..at].contains(':') {
                let head: String = token[..pos + 3].to_string();
                let tail: String = token[pos + 3 + at..].to_string();
                return format!("{head}[REDACTED]{tail}");
            }
        }
        // URL с query-параметром вида ?api_key=… — тоже маскируем ниже общим правилом
    }
    // 4. Имя записи Secrets Vault: api.key=значение / api.key: значение
    for name in names {
        if name.is_empty() || token.len() <= name.len() {
            continue;
        }
        if token.as_bytes()[..name.len()] == name.as_bytes()[..] {
            let next = token[name.len()..].chars().next();
            if matches!(next, Some('=') | Some(':')) {
                return mask_value(token, name.len(), "REDACTED:vault");
            }
        }
    }
    // 5. Присваивания с ключевыми словами: KEY=/TOKEN=/SECRET=/PASSWORD=/OB2H_*
    if let Some(eq) = token.find('=') {
        let key_part = &token[..eq];
        let upper = key_part.to_uppercase();
        let sensitive = upper.starts_with("OB2H_")
            || upper.ends_with("_KEY")
            || upper.ends_with("_TOKEN")
            || upper.ends_with("_SECRET")
            || upper.ends_with("_PASSWORD")
            || upper.ends_with("_PASSWD")
            || upper == "KEY"
            || upper == "TOKEN"
            || upper == "SECRET"
            || upper == "PASSWORD"
            || upper == "PASSWD";
        // URL-токены уже обработаны выше; здесь только «ключ=значение»
        if sensitive && !token.contains("://") {
            return mask_value(token, eq, "REDACTED");
        }
    }
    token.to_string()
}

/// Маскирование текста: токены разделяются whitespace, **разделители сохраняются
/// байт-в-байт** (новые строки и двойные пробелы не искажаются — daily-логи
/// остаются точным источником для хронологии/рекапа).
/// `Bearer <token>` — двух-токенный паттерн (флаг + хвост).
pub fn redact_with(names: &[String], text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_bearer = false;
    let mut i = 0; // байтовый индекс (все границы — на границах char)
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        if c.is_whitespace() {
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let start = i;
        while i < text.len() {
            let ch = text[i..].chars().next().unwrap();
            if ch.is_whitespace() {
                break;
            }
            i += ch.len_utf8();
        }
        let token = &text[start..i];
        if prev_bearer {
            out.push_str("[REDACTED:bearer]");
            prev_bearer = false;
        } else {
            let masked = mask_token(names, token);
            if masked.eq_ignore_ascii_case("bearer") {
                prev_bearer = true;
            }
            out.push_str(&masked);
        }
    }
    out
}
