//! Хронология и рекап сессий (v1.9.9, Фаза 66). Read-only поверх daily-логов
//! workspace (`daily/YYYY-MM-DD.jsonl`, строки DailyLogEntry) и session_ingest-меты.
//!
//! 66.1 — `memory_timeline`: окно записей до/после якоря (ISO-дата либо текст —
//! первый хит по содержимому).
//! 66.2/66.3 — `session_recap`: дайджест сессии через LLM с **инкрементальным
//! курсором префикса** в kv (`recap:<sid>` = {prefix, digest}): повторный вызов
//! на неизменённой сессии не делает LLM-вызов (кэш-хит), растущая сессия
//! до-суммаризуется только по хвосту. (Фазовый курсор дрима по history.jsonl
//! уже даёт skip неизменённого префикса для phase1 — дублировать не нужно.)

use serde_json::{json, Value};

use crate::db::Database;
use crate::llm::LLMClient;
use crate::memory::MemoryService;
use crate::workspace::Workspace;

pub const RECAP_SYSTEM: &str = "\
Ты — архивариус памяти личного агента. Тебе дают прежний итог сессии (если есть) \
и новые фрагменты диалога. Слей их в ОДИН компактный итог: факты, решения, \
пожелания пользователя, результаты и незавершённое. Формат — маркированный список \
на русском, каждый пункт самодостаточен. Без вступлений и заголовков.";
/// Сколько пар сессии берём в LLM-промпт максимум (бюджет токенов).
pub const RECAP_MAX_PAIRS: usize = 40;
/// Символов на реплику в промпте/extractive-фолбэке.
const SNIPPET: usize = 400;
/// Кап строк на пару в хронологии.
const TL_SNIPPET: usize = 200;
/// Кап source_ids у сохранённого рекапа.
const SOURCE_IDS_CAP: usize = 50;

/// Одна пара user/assistant из daily-лога.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub timestamp: String,
    pub session_id: String,
    pub author: Option<String>,
    pub user_text: String,
    pub assistant_text: String,
    /// Относительный источник для провенанса: "daily/2026-11-14.jsonl#L12".
    pub source_ref: String,
}

fn trim_chars(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

/// Прочитать daily-логи (файлы YYYY-MM-DD.jsonl по возрастанию дат).
/// Битые строки пропускаются; source_ref — для провенанса.
pub fn load_entries(ws: &Workspace) -> anyhow::Result<Vec<SessionEntry>> {
    let dir = ws.daily_dir();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().map(|e| e == "jsonl").unwrap_or(false)
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-')
                    .unwrap_or(false)
        })
        .collect();
    files.sort();
    let mut out = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        let rel = format!(
            "daily/{}",
            f.file_name().and_then(|s| s.to_str()).unwrap_or("?")
        );
        let text = std::fs::read_to_string(f)?;
        for (li, line) in text.lines().enumerate() {
            let Ok(e) = serde_json::from_str::<crate::workspace::DailyLogEntry>(line) else {
                continue;
            };
            let (sid, author) = e
                .meta
                .as_ref()
                .map(|m| {
                    (
                        m.get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        m.get("author")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    )
                })
                .unwrap_or_default();
            out.push(SessionEntry {
                timestamp: e.timestamp,
                session_id: sid,
                author,
                user_text: e.user_text,
                assistant_text: e.assistant_text,
                source_ref: format!("{rel}#L{}", li + 1),
            });
        }
        let _ = fi;
    }
    Ok(out)
}

/// Якорь хронологии: индекс записи. ISO-дата/дата-время — первая запись этой даты
/// / ближайшая >= момента; текст — первая запись, содержащая подстроку.
pub fn resolve_anchor(entries: &[SessionEntry], anchor: &str) -> Option<usize> {
    let a = anchor.trim();
    if a.is_empty() {
        return None;
    }
    // Текстовый якорь — приоритет: дата на 10 символов тоже текст, но мы
    // сначала пробуем дату (формат ISO).
    if let Ok(date) = chrono::NaiveDate::parse_from_str(&a[..a.len().min(10)], "%Y-%m-%d") {
        let date = date.format("%Y-%m-%d").to_string();
        return entries.iter().position(|e| e.timestamp.starts_with(&date));
    }
    let lower = a.to_lowercase();
    entries
        .iter()
        .position(|e| e.user_text.to_lowercase().contains(&lower) || e.assistant_text.to_lowercase().contains(&lower))
}

/// Окно записей вокруг индекса якоря: `before` записей ДО, сам якорь и `after`
/// записей ПОСЛЕ (исключительно). Без якоря — хвост (последние `before` записей).
pub fn window(entries: &[SessionEntry], anchor: Option<usize>, before: usize, after: usize) -> Vec<&SessionEntry> {
    let n = entries.len();
    let center = match anchor {
        Some(i) => i,
        None if n == 0 => return Vec::new(),
        None => n, // «хвост»: after не даёт ничего, before — последние записи
    };
    let start = center.saturating_sub(before);
    let end = (center + after + 1).min(n);
    entries[start..end].iter().collect()
}

/// Markdown-хронология.
pub fn render_timeline(entries: &[&SessionEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&format!(
            "## {} · sess={} · {}\n",
            e.timestamp,
            if e.session_id.is_empty() { "-" } else { &e.session_id },
            e.author.as_deref().unwrap_or("-")
        ));
        if !e.user_text.trim().is_empty() {
            out.push_str(&format!("- user: {}\n", trim_chars(&e.user_text, TL_SNIPPET)));
        }
        if !e.assistant_text.trim().is_empty() {
            out.push_str(&format!("- agent: {}\n", trim_chars(&e.assistant_text, TL_SNIPPET)));
        }
        out.push('\n');
    }
    out
}

fn turns_block(pairs: &[&SessionEntry]) -> String {
    pairs
        .iter()
        .map(|p| {
            format!(
                "Пользователь: {}\nАгент: {}\n",
                trim_chars(&p.user_text, SNIPPET),
                trim_chars(&p.assistant_text, SNIPPET)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extractive-фолбэк рекапа (детерминированный, без LLM): первые/последние реплики.
fn extractive_digest(pairs: &[&SessionEntry]) -> String {
    let mut lines = Vec::new();
    for p in pairs.iter().take(3) {
        if !p.user_text.trim().is_empty() {
            lines.push(format!("- Пользователь ({}): {}", p.timestamp, trim_chars(&p.user_text, SNIPPET)));
        }
    }
    if let Some(last) = pairs.last() {
        if !last.assistant_text.trim().is_empty() {
            lines.push(format!("- Агент (итог, {}): {}", last.timestamp, trim_chars(&last.assistant_text, SNIPPET)));
        }
    }
    lines.join("\n")
}

/// Рекап сессии (66.2) с инкрементальным курсором префикса (66.3).
/// `session_id` None → последняя сессия в логах (последний непустой session_id).
/// Курсор kv `recap:<sid>` = {prefix, digest}: неизменённая сессия — кэш-хит
/// без LLM; растущая — LLM получает прежний итог + хвост.
pub async fn recap_session(
    ws: &Workspace,
    db: &Database,
    llm: &dyn LLMClient,
    memory: Option<&MemoryService>,
    session_id: Option<&str>,
    save: bool,
) -> anyhow::Result<Value> {
    let entries = load_entries(ws)?;
    let sid: String = match session_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => entries
            .iter()
            .rev()
            .find(|e| !e.session_id.is_empty())
            .map(|e| e.session_id.clone())
            .unwrap_or_default(),
    };
    let mut pairs: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| e.session_id == sid)
        .collect();
    if sid.is_empty() {
        // Сессии без session_id (session_log) — берём записи последнего дня.
        if let Some(last_day) = entries.last().map(|e| e.timestamp[..10].to_string()) {
            pairs.retain(|e| e.timestamp.starts_with(&last_day));
        }
    }
    if pairs.is_empty() {
        return Ok(json!({
            "session_id": sid,
            "error": "записей сессии не найдено [Подсказка: проверьте session_id или наполните daily-логи]",
        }));
    }

    let kv_key = format!("recap:{sid}");
    let (mut prefix, mut digest): (usize, String) = db
        .get_kv(&kv_key)?
        .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        .map(|v| {
            (
                v["prefix"].as_u64().unwrap_or(0) as usize,
                v["digest"].as_str().unwrap_or("").to_string(),
            )
        })
        .unwrap_or((0, String::new()));

    let mut cached = false;
    if prefix >= pairs.len() && !digest.is_empty() {
        // 66.3: неизменённая сессия — кэш-хит, LLM-вызова нет
        cached = true;
    } else {
        let new_pairs: Vec<&SessionEntry> = pairs
            .iter()
            .skip(prefix)
            .take(RECAP_MAX_PAIRS)
            .copied()
            .collect();
        let prompt = if digest.is_empty() {
            format!(
                "Сессия ({}) пар диалога:\n\n{}\n\nИтог сессии:",
                pairs.len(),
                turns_block(&new_pairs)
            )
        } else {
            format!(
                "Прежний итог сессии:\n{}\n\nНовые фрагменты ({} пар):\n\n{}\n\nСлей прежний итог и новые фрагменты в один обновлённый итог:",
                digest,
                new_pairs.len(),
                turns_block(&new_pairs)
            )
        };
        digest = match llm.ask(&prompt, Some(RECAP_SYSTEM)).await {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            Ok(_) | Err(_) => extractive_digest(&pairs),
        };
        prefix = (prefix + new_pairs.len()).min(pairs.len());
        db.set_kv(
            &kv_key,
            &json!({ "prefix": prefix, "digest": digest, "pairs_total": pairs.len() }).to_string(),
        )?;
    }

    let mut out = json!({
        "session_id": sid,
        "pairs_total": pairs.len(),
        "summarized_prefix": prefix,
        "cached": cached,
        "digest": digest,
    });

    if save {
        let Some(memory) = memory else {
            out["save_error"] = json!("memory-сервис недоступен");
            return Ok(out);
        };
        let source_ids: Vec<String> = pairs
            .iter()
            .take(SOURCE_IDS_CAP)
            .map(|p| p.source_ref.clone())
            .collect();
        let author = pairs.first().and_then(|p| p.author.clone());
        let mut meta = json!({ "session_id": sid, "source_ids": source_ids });
        if let Some(a) = author {
            meta["author"] = json!(a);
        }
        match memory
            .save(&digest, None, "recap", 0.6, "recap", Some(&meta.to_string()))
            .await
        {
            Ok(key) => {
                out["saved_key"] = json!(key);
            }
            Err(e) => {
                out["save_error"] = json!(e.to_string());
            }
        }
    }
    Ok(out)
}
