//! Слоты рабочего контекста (slots, Фаза 65): редактируемые агентом единицы
//! памяти с жёстким лимитом размера, признаком `pinned` и скоупом
//! project/global. Семантика по мотивам rohitg00/agentmemory slots
//! (см. PLAN_v1.9.9 §3.3/§5.3), адаптирована под OB2H:
//! - project-слот затеняет global-слот с тем же label (чтение и запись);
//! - append при переполнении — 413-семантика (скомпактируйте через slot_replace);
//! - инъекция pinned-слотов в контекст — `pinned_context()` (Ф65.3);
//! - seed-дефолты §5.3 создаются лениво (`ensure_defaults`).
//!
//! Доступ к БД — ручной лок `conn_arc()` (паттерн SecretsService): доменные
//! ошибки — anyhow с `[Error] … [Подсказка]`, как требует AGENTS.md §6.
//! Слоты не попадают в векторный индекс — это контекст, а не факты (осознанно).

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::audit;
use super::timeline::SessionEntry;
use crate::db::{utcnow, Database};
use crate::workspace::Workspace;

/// Дефолтный лимит содержимого слота (символов).
pub const DEFAULT_SIZE_LIMIT: i64 = 2000;
/// Жёсткий потолок size_limit.
pub const SIZE_LIMIT_CAP: i64 = 20000;
/// Бюджет инъекции слотов в контекст (env, символы).
pub const SLOTS_CONTEXT_BUDGET_ENV: &str = "OB2H_SLOTS_MAX_CHARS";
pub const SLOTS_CONTEXT_BUDGET_DEFAULT: usize = 1500;
/// Кап длины label.
const LABEL_MAX: usize = 64;

#[derive(Debug, Clone, Serialize)]
pub struct SlotRecord {
    pub label: String,
    /// '' = global; иначе project_id. В выдаче дублируется полем `scope`.
    pub project: String,
    /// project | global (удобство для MCP-клиентов).
    pub scope: String,
    pub content: String,
    pub size_limit: i64,
    pub description: String,
    pub pinned: bool,
    pub read_only: bool,
    pub created_at: String,
    pub updated_at: String,
}

pub struct SlotsService {
    db: Database,
}

/// label: lowercase, начинается с буквы, [a-z0-9_], до 64 символов.
pub fn validate_label(label: &str) -> anyhow::Result<String> {
    let t = label.trim();
    if t.is_empty() || t.chars().count() > LABEL_MAX {
        anyhow::bail!("[Error] label слота пуст или длиннее {LABEL_MAX} символов [Подсказка: короткое имя вида pending_items]");
    }
    let mut chars = t.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        anyhow::bail!("[Error] label слота должен начинаться со строчной латинской буквы [Подсказка: pending_items, session_guidance, …]");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        anyhow::bail!("[Error] недопустимые символы в label слота [Подсказка: разрешены a-z, 0-9 и _]");
    }
    Ok(t.to_string())
}

fn scope_of(project: &str) -> &'static str {
    if project.is_empty() {
        "global"
    } else {
        "project"
    }
}

/// Инъекция pinned-слотов в контекст (Ф65.3): непустые pinned-слоты проекта
/// (project-слот затеняет global), алфавитный порядок, бюджет
/// `OB2H_SLOTS_MAX_CHARS` (дефолт 1500) — заполнение по границе целого слота.
/// Чистое чтение: ничего не пишет (правило read-only viewer/memory-context).
pub fn pinned_context(conn: &Connection, project: Option<&str>) -> String {
    let budget = std::env::var(SLOTS_CONTEXT_BUDGET_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(SLOTS_CONTEXT_BUDGET_DEFAULT);
    if budget == 0 {
        return String::new();
    }

    let mut stmt = match conn.prepare(
        "SELECT label, project, content FROM slots \
         WHERE pinned = 1 AND TRIM(content) <> '' AND (project = '' OR project = ?1)",
    ) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let rows: Vec<(String, String, String)> = stmt
        .query_map(params![project.unwrap_or("")], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map(|it| it.flatten().collect())
        .unwrap_or_default();

    // project-слот затеняет global с тем же label (порядок строк не важен)
    let mut by_label: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (label, project, content) in rows {
        let is_project = !project.is_empty();
        if is_project || !by_label.contains_key(&label) {
            by_label.insert(label, content);
        }
    }
    if by_label.is_empty() {
        return String::new();
    }

    let mut out = String::from("<agent_slots>\n");
    let mut used = out.len();
    for (label, content) in &by_label {
        let chunk = format!("## {label}\n{}\n\n", content.trim());
        if used + chunk.len() > budget {
            // не влезает целиком — пропускаем (без обрыва внутри слота),
            // но более короткие слоты ниже по алфавиту всё ещё попадают
            continue;
        }
        out.push_str(&chunk);
        used += chunk.len();
    }
    if out == "<agent_slots>\n" {
        return String::new();
    }
    out.push_str("</agent_slots>");
    out
}

impl SlotsService {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Маппинг строки; порядок колонок — `Self::COLS`.
    fn row_to_slot(row: &rusqlite::Row<'_>) -> rusqlite::Result<SlotRecord> {
        let project: String = row.get(1)?;
        let scope = scope_of(&project).to_string();
        Ok(SlotRecord {
            label: row.get(0)?,
            project,
            scope,
            content: row.get(2)?,
            size_limit: row.get(3)?,
            description: row.get(4)?,
            pinned: row.get::<_, i64>(5)? != 0,
            read_only: row.get::<_, i64>(6)? != 0,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    const COLS: &'static str = "label, project, content, size_limit, description, pinned, \
         read_only, created_at, updated_at";

    /// Чтение с затенением по открытому соединению: project-слот, иначе global.
    fn get_in_conn(
        conn: &Connection,
        label: &str,
        project: Option<&str>,
    ) -> rusqlite::Result<Option<SlotRecord>> {
        let sql = format!("SELECT {} FROM slots WHERE label = ?1 AND project = ?2", Self::COLS);
        if let Some(p) = project {
            if let Some(s) = conn
                .query_row(&sql, params![label, p], Self::row_to_slot)
                .optional()?
            {
                return Ok(Some(s));
            }
        }
        conn.query_row(&sql, params![label, ""], Self::row_to_slot)
            .optional()
    }

    fn fetch(conn: &Connection, label: &str, project: &str) -> anyhow::Result<SlotRecord> {
        conn.query_row(
            &format!("SELECT {} FROM slots WHERE label = ?1 AND project = ?2", Self::COLS),
            params![label, project],
            Self::row_to_slot,
        )
        .map_err(|e| anyhow::anyhow!("[Error] слот \"{label}\" не прочитан после записи: {e}"))
    }

    /// Ленивые seed-дефолты §5.3 (по одному набору на проект; global — один на узел).
    pub fn ensure_defaults(&self, project: &str) -> anyhow::Result<()> {
        let now = utcnow();
        let conn_arc = self.db.conn_arc();
        let conn = conn_arc
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        for (label, scope, limit, desc, pinned) in [
            ("pending_items", "project", 2000, "Незавершённые задачи, обещания, TODO", true),
            ("session_guidance", "project", 1500, "Фокус и риски на следующую сессию", true),
            ("project_context", "project", 3000, "Команды сборки/теста, конвенции репозитория", true),
            ("user_rules", "global", 1500, "Стойкие правила инструментария пользователя", true),
            ("self_notes", "project", 1500, "Гипотезы, тупики, что перепроверить (не инъектируется)", false),
        ] {
            let proj = if scope == "global" { "" } else { project };
            conn.execute(
                "INSERT OR IGNORE INTO slots (label, project, size_limit, description, pinned, \
                 created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![label, proj, limit, desc, pinned as i64, now],
            )?;
        }
        Ok(())
    }

    /// Список слотов: global + указанный проект; project-слот затеняет global
    /// с тем же label. Сортировка по label.
    pub fn list(&self, project: Option<&str>) -> anyhow::Result<Vec<SlotRecord>> {
        let conn_arc = self.db.conn_arc();
        let conn = conn_arc
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        let sql = format!(
            "SELECT {} FROM slots WHERE project = '' OR project = ?1 ORDER BY label",
            Self::COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![project.unwrap_or("")], Self::row_to_slot)?
            .flatten()
            .collect::<Vec<SlotRecord>>();
        // затенение: по одному слоту на label (приоритет project)
        let mut best: std::collections::BTreeMap<String, SlotRecord> =
            std::collections::BTreeMap::new();
        for r in rows {
            let is_project = r.scope == "project";
            match best.get(&r.label) {
                Some(prev) if prev.scope == "project" && !is_project => continue,
                _ => {
                    best.insert(r.label.clone(), r);
                }
            }
        }
        Ok(best.into_values().collect())
    }

    /// Чтение с затенением: project-слот, иначе global.
    pub fn get(&self, label: &str, project: Option<&str>) -> anyhow::Result<SlotRecord> {
        let label = validate_label(label)?;
        let conn_arc = self.db.conn_arc();
        let conn = conn_arc
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        Self::get_in_conn(&conn, &label, project)?.ok_or_else(|| {
            anyhow::anyhow!("[Error] слот \"{label}\" не найден [Подсказка: создайте через slot_create или посмотрите slot_list]")
        })
    }

    /// Создать слот. Дубликат label в том же скоупе запрещён (project-слот
    /// может затенять global — проверка локальна для скоупа).
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        label: &str,
        content: &str,
        size_limit: Option<i64>,
        description: Option<&str>,
        pinned: bool,
        scope: &str,
        project: Option<&str>,
        actor: &str,
        tool: &str,
    ) -> anyhow::Result<SlotRecord> {
        let label = validate_label(label)?;
        let limit = size_limit.unwrap_or(DEFAULT_SIZE_LIMIT);
        if !(1..=SIZE_LIMIT_CAP).contains(&limit) {
            anyhow::bail!("[Error] size_limit должен быть 1..{SIZE_LIMIT_CAP}, получено {limit}");
        }
        let content_len = content.chars().count() as i64;
        if content_len > limit {
            anyhow::bail!("[Error] содержимое превышает size_limit ({content_len} > {limit}) [Подсказка: увеличьте size_limit или сократите текст]");
        }
        let proj = match scope {
            "global" => "",
            "project" => project.unwrap_or_default(),
            other => anyhow::bail!("[Error] scope должен быть 'project' или 'global', получено '{other}'"),
        };
        if scope == "project" && proj.is_empty() {
            anyhow::bail!("[Error] для scope=project не определён проект [Подсказка: передайте project или откройте сессию в репозитории]");
        }
        let scope_name = scope_of(proj);
        let now = utcnow();

        let conn_arc = self.db.conn_arc();
        let conn = conn_arc
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT label FROM slots WHERE label = ?1 AND project = ?2",
                params![label, proj],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            anyhow::bail!("[Error] слот \"{label}\" уже существует в скоупе {scope_name} [Подсказка: правьте через slot_append/slot_replace]");
        }
        conn.execute(
            "INSERT INTO slots (label, project, content, size_limit, description, pinned, \
             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![label, proj, content, limit, description.unwrap_or(""), pinned as i64, now],
        )?;
        audit::record(
            &conn,
            actor,
            "slot_create",
            std::slice::from_ref(&label),
            serde_json::json!({ "scope": scope_name, "size_limit": limit, "pinned": pinned }),
            tool,
        )?;
        Self::fetch(&conn, &label, proj)
    }

    /// Дописать текст. Переполнение — 413-семантика: отказ с текущим размером
    /// и подсказкой «скомпактируйте через slot_replace».
    pub fn append(
        &self,
        label: &str,
        project: Option<&str>,
        text: &str,
        actor: &str,
        tool: &str,
    ) -> anyhow::Result<SlotRecord> {
        let label = validate_label(label)?;
        let now = utcnow();
        let conn_arc = self.db.conn_arc();
        let conn = conn_arc
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        let slot = Self::get_in_conn(&conn, &label, project)?.ok_or_else(|| {
            anyhow::anyhow!("[Error] слот \"{label}\" не найден [Подсказка: создайте через slot_create]")
        })?;
        if slot.read_only {
            anyhow::bail!("[Error] слот \"{label}\" read-only [Подсказка: этот слот защищён от записи]");
        }
        let sep = if slot.content.is_empty() || slot.content.ends_with('\n') { "" } else { "\n" };
        let next = format!("{}{}{}", slot.content, sep, text);
        let next_len = next.chars().count() as i64;
        if next_len > slot.size_limit {
            anyhow::bail!(
                "[Error] append превышает sizeLimit ({next_len} > {}) [Подсказка: скомпактируйте содержимое через slot_replace (текущий размер {}/{}, после добавления {})]",
                slot.size_limit,
                slot.content.chars().count(),
                slot.size_limit,
                next_len
            );
        }
        conn.execute(
            "UPDATE slots SET content = ?1, updated_at = ?2 WHERE label = ?3 AND project = ?4",
            params![next, now, slot.label, slot.project],
        )?;
        audit::record(
            &conn,
            actor,
            "slot_append",
            std::slice::from_ref(&slot.label),
            serde_json::json!({ "scope": slot.scope, "added": text.chars().count(), "total": next_len }),
            tool,
        )?;
        Self::fetch(&conn, &slot.label, &slot.project)
    }

    /// Заменить содержимое целиком (компактизация). Лимит проверяется.
    pub fn replace(
        &self,
        label: &str,
        project: Option<&str>,
        content: &str,
        actor: &str,
        tool: &str,
    ) -> anyhow::Result<SlotRecord> {
        let label = validate_label(label)?;
        let now = utcnow();
        let conn_arc = self.db.conn_arc();
        let conn = conn_arc
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        let slot = Self::get_in_conn(&conn, &label, project)?.ok_or_else(|| {
            anyhow::anyhow!("[Error] слот \"{label}\" не найден [Подсказка: создайте через slot_create]")
        })?;
        if slot.read_only {
            anyhow::bail!("[Error] слот \"{label}\" read-only [Подсказка: этот слот защищён от записи]");
        }
        let new_len = content.chars().count() as i64;
        if new_len > slot.size_limit {
            anyhow::bail!(
                "[Error] содержимое превышает sizeLimit ({new_len} > {}) [Подсказка: текущий размер {}/{}]",
                slot.size_limit,
                slot.content.chars().count(),
                slot.size_limit
            );
        }
        conn.execute(
            "UPDATE slots SET content = ?1, updated_at = ?2 WHERE label = ?3 AND project = ?4",
            params![content, now, slot.label, slot.project],
        )?;
        audit::record(
            &conn,
            actor,
            "slot_replace",
            std::slice::from_ref(&slot.label),
            serde_json::json!({ "scope": slot.scope, "before": slot.content.chars().count(), "after": new_len }),
            tool,
        )?;
        Self::fetch(&conn, &slot.label, &slot.project)
    }

    /// Удалить слот (тот, на который указывает затенение). read_only защищён.
    pub fn delete(
        &self,
        label: &str,
        project: Option<&str>,
        actor: &str,
        tool: &str,
    ) -> anyhow::Result<bool> {
        let label = validate_label(label)?;
        let conn_arc = self.db.conn_arc();
        let conn = conn_arc
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock error: {e}"))?;
        let slot = match Self::get_in_conn(&conn, &label, project)? {
            Some(s) => s,
            None => return Ok(false),
        };
        if slot.read_only {
            anyhow::bail!("[Error] слот \"{label}\" read-only [Подсказка: этот слот защищён от удаления]");
        }
        conn.execute(
            "DELETE FROM slots WHERE label = ?1 AND project = ?2",
            params![slot.label, slot.project],
        )?;
        audit::record(
            &conn,
            actor,
            "slot_delete",
            std::slice::from_ref(&slot.label),
            serde_json::json!({ "scope": slot.scope }),
            tool,
        )?;
        Ok(true)
    }
}

/// Ф66.4 (OB2H_DREAM_SLOTS, дефолт off): dream-слот-рефлекс — порт
/// `slot-reflect` agentmemory (§3.3). Свежие TODO из daily-логов текущего дня
/// → global `pending_items` (append недостающих, cap 20 строк); счётчики дня
/// (пары диалога, строки с ошибками) → global `session_guidance` (replace).
/// Global-скоуп — у дрима нет проектного контекста (осознанно; записано в
/// PLAN.md §6).
pub fn dream_reflect(ws: &Workspace, db: &Database) -> anyhow::Result<serde_json::Value> {
    let svc = SlotsService::new(db.clone());
    let entries = crate::lifecycle::timeline::load_entries(ws)?;
    let today = utcnow()[..10].to_string();
    let todays: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| e.timestamp.starts_with(&today))
        .collect();

    let mut todos: Vec<String> = Vec::new();
    let mut error_lines = 0usize;
    for e in &todays {
        for text in [&e.user_text, &e.assistant_text] {
            for line in text.lines() {
                let lower = line.to_lowercase();
                if lower.contains("todo") {
                    todos.push(line.trim().chars().take(200).collect());
                }
                if lower.contains("ошибка") || lower.contains("error") {
                    error_lines += 1;
                }
            }
        }
    }
    todos.sort();
    todos.dedup();

    // pending_items: append недостающих строк (глобальный скоуп, создаётся при нужде)
    let mut appended = 0usize;
    let existing = svc
        .get("pending_items", None)
        .ok()
        .map(|s| s.content)
        .unwrap_or_default();
    let fresh: Vec<String> = todos
        .iter()
        .filter(|t| !existing.contains(t.as_str()))
        .take(20)
        .cloned()
        .collect();
    if !fresh.is_empty() {
        let add = fresh.join("\n");
        let res = if existing.trim().is_empty() {
            svc.create(
                "pending_items",
                &add,
                None,
                Some("Незавершённые задачи, обещания, TODO"),
                true,
                "global",
                None,
                "dream",
                "dream_reflect",
            )
            .map(|_| ())
        } else {
            svc.append("pending_items", None, &add, "dream", "dream_reflect")
                .map(|_| ())
        };
        // переполнение слота не роняет рефлекс: остальное доедет в следующий дрим
        if res.is_ok() {
            appended = fresh.len();
        }
    }

    // session_guidance: replace со счётчиками дня (глобальный скоуп)
    let summary = format!(
        "последний рефлекс: {}\n- пар диалога сегодня: {}\n- строк с упоминанием ошибок: {}\n- TODO найдено: {}",
        utcnow(),
        todays.len(),
        error_lines,
        todos.len()
    );
    let guided = if svc.get("session_guidance", None).is_ok() {
        svc.replace("session_guidance", None, &summary, "dream", "dream_reflect")
    } else {
        svc.create(
            "session_guidance",
            &summary,
            None,
            Some("Счётчики паттернов дня (из дрима)"),
            true,
            "global",
            None,
            "dream",
            "dream_reflect",
        )
    };

    Ok(serde_json::json!({
        "entries_scanned": todays.len(),
        "todos_found": todos.len(),
        "todos_appended": appended,
        "guidance_updated": guided.is_ok(),
    }))
}
