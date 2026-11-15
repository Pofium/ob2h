//! Фаза 74 (PLAN_v2.0): автономное разрешение конфликтов и актуализация (Dream).
//!
//! - 74.1: Трёхуровневая классификация конфликтов:
//!   `TemporalSupersession`, `UserCorrection`, `ContextualCoexistence`.
//! - 74.2: Блокинг кандидатов по `category` + обязательному Entity/Subject Overlap
//!   + семантический порог `cosine > 0.82`.
//! - 74.3: Dual-track детектор: Online (`LLMClient`) и Offline (эвристики без сети).
//! - 74.4: Рёбра `SUPERSEDES`/`CONTRADICTS`, `valid_until`, tombstone `status='SUPERSEDED'`
//!   без физического удаления; git-anchored facts (`git_head_sha`).
//! - 74.5: HITL-предпросмотр и применение (`knowledge_reconcile`).
//! - 74.6: Жёсткий бюджет ночного цикла (`max_tokens`, `max_duration`).
//! - 74.7: Идемпотентность и автоматический откат транзакции при деградации метрик (ADR-13).

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::db::{utcnow, Database};
use crate::dream::bench_gate::is_degradation;
use crate::llm::{clean_json_markdown, LLMClient};
use crate::vector::similarity::{cosine, deserialize};

/// Порог косинусной близости для фильтрации кандидатов конфликта (74.2).
pub const CONFLICT_COSINE_THRESHOLD: f32 = 0.82;

/// Трёхуровневая классификация конфликтов (74.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictClass {
    TemporalSupersession,
    UserCorrection,
    ContextualCoexistence,
}

impl ConflictClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TemporalSupersession => "temporal_supersession",
            Self::UserCorrection => "user_correction",
            Self::ContextualCoexistence => "contextual_coexistence",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "temporal_supersession" | "supersedes" | "temporal" => Some(Self::TemporalSupersession),
            "user_correction" | "correction" | "contradicts" => Some(Self::UserCorrection),
            "contextual_coexistence" | "coexistence" | "coexists" | "keep_both" => {
                Some(Self::ContextualCoexistence)
            }
            _ => None,
        }
    }
}

/// Кандидат на проверку конфликтов.
#[derive(Debug, Clone)]
pub struct ConflictCandidate {
    pub id: i64,
    pub key: String,
    pub content: String,
    pub category: String,
    pub meta: String,
    pub importance: f64,
    pub trust: f64,
    pub updated_at: String,
    pub embedding: Option<Vec<f32>>,
    pub git_head_sha: Option<String>,
}

/// Решение по паре конфликтующих записей.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub older_id: i64,
    pub newer_id: i64,
    pub older_key: String,
    pub newer_key: String,
    pub class: ConflictClass,
    /// `"SUPERSEDES"` | `"CONTRADICTS"` | `"COEXISTS"`
    pub relation: String,
    pub reason: String,
    pub git_priority: bool,
}

/// Жёсткий бюджет ночного цикла Dream (74.6).
#[derive(Debug, Clone, Copy)]
pub struct DreamBudget {
    pub max_tokens: usize,
    pub max_duration: Duration,
}

impl Default for DreamBudget {
    fn default() -> Self {
        Self {
            max_tokens: 32_000,
            max_duration: Duration::from_secs(120),
        }
    }
}

impl DreamBudget {
    pub fn from_env() -> Self {
        let max_tokens = std::env::var("OB2H_DREAM_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(32_000);
        let max_secs = std::env::var("OB2H_DREAM_MAX_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(120);
        Self {
            max_tokens,
            max_duration: Duration::from_secs(max_secs),
        }
    }
}

/// Отчёт сверки знаний (`knowledge_reconcile` и ночной цикл).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub preview_only: bool,
    pub candidates_checked: usize,
    pub pairs_after_blocking: usize,
    pub resolutions: Vec<ConflictResolution>,
    pub edges_created: usize,
    pub superseded_count: usize,
    pub tokens_used: usize,
    pub budget_exhausted: bool,
    pub rolled_back: bool,
}

/// Текущий HEAD SHA git-репозитория (74.4).
pub fn current_git_head_sha(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// Проверка, изменился ли привязанный файл с момента `fact_sha` (74.4).
/// Пустой diff гарантирует актуальность факта; непустой даёт приоритет проверки.
pub fn git_file_changed_since(repo_root: &Path, fact_sha: &str, file_path: &str) -> bool {
    if fact_sha.trim().is_empty() || file_path.trim().is_empty() {
        return false;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["diff", "--quiet", &format!("{fact_sha}..HEAD"), "--", file_path])
        .status();
    match out {
        Ok(st) => !st.success(),
        Err(_) => false,
    }
}

fn meta_str(meta: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(meta).ok()?;
    v.get(field)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
}

/// Извлечь нормализованные сущности/субъекты для Entity/Subject Overlap (74.2).
pub fn extract_named_entities(cand: &ConflictCandidate) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cand.meta) {
        for field in ["file_path", "subject", "entity"] {
            if let Some(s) = v.get(field).and_then(|x| x.as_str()) {
                let norm = s.trim().to_lowercase();
                if !norm.is_empty() {
                    out.insert(norm);
                }
            }
        }
        if let Some(arr) = v.get("entities").and_then(|x| x.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    let norm = s.trim().to_lowercase();
                    if !norm.is_empty() {
                        out.insert(norm);
                    }
                }
            }
        }
    }
    // Именованные идентификаторы из текста: CamelCase / snake_case / пути с `/` или `.`
    for raw in cand.content.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '(' | ')' | '"' | '\'')) {
        let tok = raw.trim();
        if tok.len() < 2 {
            continue;
        }
        let has_upper = tok.chars().any(|c| c.is_uppercase());
        let has_path = tok.contains('/') || tok.contains('_') || tok.contains('.');
        if has_upper || has_path {
            out.insert(tok.to_lowercase());
        }
    }
    out
}

/// Предварительная фильтрация пар кандидатов (74.2):
/// 1) одинаковая категория (блокинг);
/// 2) совместимый `project_id` (если задан);
/// 3) обязательный Entity/Subject Overlap (разные объекты отсекаются ДО косинуса);
/// 4) `cosine > 0.82` (если переданы эмбеддинги).
pub fn filter_conflict_candidates(
    candidates: &[ConflictCandidate],
    require_cosine: bool,
) -> Vec<(usize, usize)> {
    let entities: Vec<HashSet<String>> = candidates.iter().map(extract_named_entities).collect();
    let projects: Vec<Option<String>> = candidates
        .iter()
        .map(|c| meta_str(&c.meta, "project_id"))
        .collect();

    let mut pairs = Vec::new();
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            let a = &candidates[i];
            let b = &candidates[j];

            if a.category != b.category {
                continue;
            }
            if let (Some(pa), Some(pb)) = (&projects[i], &projects[j]) {
                if pa != pb {
                    continue;
                }
            }

            // Обязательный Entity/Subject Overlap: если у обеих записей есть именованные
            // сущности, их пересечение не должно быть пустым («Auth» vs «Billing» -> skip).
            if !entities[i].is_empty() && !entities[j].is_empty() && entities[i].is_disjoint(&entities[j]) {
                continue;
            }
            if entities[i].is_empty() != entities[j].is_empty() {
                // Одна запись привязана к конкретной сущности, другая нет — проверяем подстроку.
                let (ent_set, other_text) = if !entities[i].is_empty() {
                    (&entities[i], b.content.to_lowercase())
                } else {
                    (&entities[j], a.content.to_lowercase())
                };
                if !ent_set.iter().any(|e| other_text.contains(e)) {
                    continue;
                }
            }

            if require_cosine {
                match (&a.embedding, &b.embedding) {
                    (Some(ea), Some(eb)) if ea.len() == eb.len() && !ea.is_empty() => {
                        if cosine(ea, eb) <= CONFLICT_COSINE_THRESHOLD {
                            continue;
                        }
                    }
                    _ => continue,
                }
            }
            pairs.push((i, j));
        }
    }
    pairs
}

fn context_envs(text: &str) -> HashSet<&'static str> {
    let lower = text.to_lowercase();
    let mut envs = HashSet::new();
    for (kw, tag) in [
        ("prod", "prod"),
        ("production", "prod"),
        ("dev", "dev"),
        ("development", "dev"),
        ("staging", "staging"),
        ("local", "local"),
        ("vps", "vps"),
        ("windows", "windows"),
        ("linux", "linux"),
        ("macos", "macos"),
    ] {
        if lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == kw)
        {
            envs.insert(tag);
        }
    }
    envs
}

fn extract_numbers(text: &str) -> Vec<String> {
    let mut nums = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            nums.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        nums.push(cur);
    }
    nums
}

/// Offline-детектор противоречий без обращения к сети (74.3).
pub fn detect_conflict_offline(
    a: &ConflictCandidate,
    b: &ConflictCandidate,
    git_priority: bool,
) -> Option<ConflictResolution> {
    let (older, newer) = if a.updated_at <= b.updated_at {
        (a, b)
    } else {
        (b, a)
    };
    if older.content.trim() == newer.content.trim() {
        return None;
    }

    // 1. Contextual Coexistence: разные окружения/контексты («в dev X, в prod Y»).
    let envs_old = context_envs(&older.content);
    let envs_new = context_envs(&newer.content);
    if !envs_old.is_empty() && !envs_new.is_empty() && envs_old.is_disjoint(&envs_new) {
        return Some(ConflictResolution {
            older_id: older.id,
            newer_id: newer.id,
            older_key: older.key.clone(),
            newer_key: newer.key.clone(),
            class: ConflictClass::ContextualCoexistence,
            relation: "COEXISTS".to_string(),
            reason: "Разные контексты/окружения — обе записи сохраняются".to_string(),
            git_priority,
        });
    }

    // 2. User Correction: явное опровержение/исправление в новой записи.
    let lower_new = newer.content.to_lowercase();
    let correction_markers = [
        "не ",
        "вместо ",
        "исправ",
        "на самом деле",
        "ошиб",
        "отмен",
        "not ",
        "instead",
        "actually",
        "wrong",
    ];
    if correction_markers.iter().any(|m| lower_new.contains(m)) {
        return Some(ConflictResolution {
            older_id: older.id,
            newer_id: newer.id,
            older_key: older.key.clone(),
            newer_key: newer.key.clone(),
            class: ConflictClass::UserCorrection,
            relation: "CONTRADICTS".to_string(),
            reason: "Явное исправление/опровержение в более новой записи".to_string(),
            git_priority,
        });
    }

    // 3. Temporal Supersession: изменение числовых значений/версий/таймаутов/портов.
    let nums_old = extract_numbers(&older.content);
    let nums_new = extract_numbers(&newer.content);
    if !nums_old.is_empty() && !nums_new.is_empty() && nums_old != nums_new {
        return Some(ConflictResolution {
            older_id: older.id,
            newer_id: newer.id,
            older_key: older.key.clone(),
            newer_key: newer.key.clone(),
            class: ConflictClass::TemporalSupersession,
            relation: "SUPERSEDES".to_string(),
            reason: "Обновление параметров/значений во времени".to_string(),
            git_priority,
        });
    }

    // Общий случай изменения факта с общим субъектом.
    Some(ConflictResolution {
        older_id: older.id,
        newer_id: newer.id,
        older_key: older.key.clone(),
        newer_key: newer.key.clone(),
        class: ConflictClass::TemporalSupersession,
        relation: "SUPERSEDES".to_string(),
        reason: "Более свежая запись замещает предыдущую формулировку".to_string(),
        git_priority,
    })
}

#[derive(Deserialize)]
struct LlmConflictVerdict {
    class: Option<String>,
    reason: Option<String>,
}

/// Online-детектор через `LLMClient` с fallback на offline-эвристики (74.3).
pub async fn detect_conflict_online(
    llm: &dyn LLMClient,
    a: &ConflictCandidate,
    b: &ConflictCandidate,
    git_priority: bool,
) -> (Option<ConflictResolution>, usize) {
    let (older, newer) = if a.updated_at <= b.updated_at {
        (a, b)
    } else {
        (b, a)
    };
    let sys = "Классифицируй конфликт двух фактов. Верни СТРОГО JSON: \
{\"class\": \"temporal_supersession|user_correction|contextual_coexistence|none\", \"reason\": \"кратко\"}.";
    let usr = format!(
        "Старый факт ({}, {}): {}\nНовый факт ({}, {}): {}",
        older.key, older.updated_at, older.content, newer.key, newer.updated_at, newer.content
    );
    let est_tokens = (sys.len() + usr.len()) / 4 + 64;
    match llm.ask(&usr, Some(sys)).await {
        Ok(raw) => {
            let cleaned = clean_json_markdown(&raw);
            if let Ok(v) = serde_json::from_str::<LlmConflictVerdict>(&cleaned) {
                if let Some(cls_str) = v.class.as_deref() {
                    if cls_str.trim().eq_ignore_ascii_case("none") {
                        return (None, est_tokens);
                    }
                    if let Some(cls) = ConflictClass::parse(cls_str) {
                        let relation = match cls {
                            ConflictClass::TemporalSupersession => "SUPERSEDES",
                            ConflictClass::UserCorrection => "CONTRADICTS",
                            ConflictClass::ContextualCoexistence => "COEXISTS",
                        };
                        return (
                            Some(ConflictResolution {
                                older_id: older.id,
                                newer_id: newer.id,
                                older_key: older.key.clone(),
                                newer_key: newer.key.clone(),
                                class: cls,
                                relation: relation.to_string(),
                                reason: v.reason.unwrap_or_else(|| cls.as_str().to_string()),
                                git_priority,
                            }),
                            est_tokens,
                        );
                    }
                }
            }
            (detect_conflict_offline(a, b, git_priority), est_tokens)
        }
        Err(e) => {
            warn!("Online conflict LLM fallback to offline: {e}");
            (detect_conflict_offline(a, b, git_priority), 0)
        }
    }
}

/// Применение резолюций в БД: рёбра `SUPERSEDES`/`CONTRADICTS`, `valid_until` и
/// tombstone-статус `'SUPERSEDED'` без физического удаления (74.4, идемпотентно).
pub fn apply_reconcile_resolutions(
    conn: &Connection,
    resolutions: &[ConflictResolution],
) -> rusqlite::Result<(usize, usize)> {
    let now = utcnow();
    let mut edges_created = 0usize;
    let mut superseded_count = 0usize;

    for r in resolutions {
        match r.class {
            ConflictClass::ContextualCoexistence => {
                // Обе записи остаются ACTIVE.
            }
            ConflictClass::TemporalSupersession => {
                let inserted = conn.execute(
                    "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, weight, created_at)
                     VALUES (?1, ?2, 'SUPERSEDES', 1.0, ?3)",
                    params![r.newer_id, r.older_id, now],
                )?;
                edges_created += inserted;
                let upd = conn.execute(
                    "UPDATE memories
                     SET status = 'SUPERSEDED',
                         valid_until = COALESCE(valid_until, ?1)
                     WHERE id = ?2 AND COALESCE(status, 'ACTIVE') != 'SUPERSEDED'",
                    params![now, r.older_id],
                )?;
                superseded_count += upd;
            }
            ConflictClass::UserCorrection => {
                let ins1 = conn.execute(
                    "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, weight, created_at)
                     VALUES (?1, ?2, 'CONTRADICTS', 1.0, ?3)",
                    params![r.newer_id, r.older_id, now],
                )?;
                let ins2 = conn.execute(
                    "INSERT OR IGNORE INTO memory_links (from_id, to_id, kind, weight, created_at)
                     VALUES (?1, ?2, 'SUPERSEDES', 1.0, ?3)",
                    params![r.newer_id, r.older_id, now],
                )?;
                edges_created += ins1 + ins2;
                let upd = conn.execute(
                    "UPDATE memories
                     SET status = 'SUPERSEDED',
                         valid_until = COALESCE(valid_until, ?1)
                     WHERE id = ?2 AND COALESCE(status, 'ACTIVE') != 'SUPERSEDED'",
                    params![now, r.older_id],
                )?;
                superseded_count += upd;
            }
        }
    }

    Ok((edges_created, superseded_count))
}

/// Загрузка активных кандидатов памяти для сверки конфликтов.
pub fn load_active_candidates(
    conn: &Connection,
    project_id: Option<&str>,
    min_importance: f64,
    limit: usize,
) -> rusqlite::Result<Vec<ConflictCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, content, COALESCE(category, 'fact'), COALESCE(meta, '{}'),
                importance, trust, updated_at, embedding, git_head_sha
         FROM memories
         WHERE deleted_at IS NULL
           AND COALESCE(status, 'ACTIVE') = 'ACTIVE'
           AND importance >= ?1
         ORDER BY updated_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![min_importance, limit as i64], |r| {
        let emb_blob: Option<Vec<u8>> = r.get(8)?;
        let emb = emb_blob.and_then(|b| deserialize(&b));
        Ok(ConflictCandidate {
            id: r.get(0)?,
            key: r.get(1)?,
            content: r.get(2)?,
            category: r.get(3)?,
            meta: r.get(4)?,
            importance: r.get(5)?,
            trust: r.get(6)?,
            updated_at: r.get(7)?,
            embedding: emb,
            git_head_sha: r.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        if let Some(pid) = project_id {
            if let Some(mpid) = meta_str(&row.meta, "project_id") {
                if mpid != pid.to_lowercase() {
                    continue;
                }
            }
        }
        out.push(row);
    }
    Ok(out)
}

/// Полный цикл сверки (preview или apply) с бюджетом и git-приоритетом (74.1–74.6).
pub async fn reconcile_knowledge(
    conn: &Connection,
    llm: Option<&dyn LLMClient>,
    project_id: Option<&str>,
    min_importance: f64,
    apply: bool,
    require_cosine: bool,
    budget: DreamBudget,
    repo_root: Option<&Path>,
) -> anyhow::Result<ReconcileReport> {
    let started = Instant::now();
    let candidates = load_active_candidates(conn, project_id, min_importance, 250)?;
    let mut pairs = filter_conflict_candidates(&candidates, require_cosine);

    // Приоритет git-anchored фактов, чей файл изменился с момента git_head_sha (74.4).
    let pair_priority = |i: usize, j: usize| -> bool {
        let Some(root) = repo_root else {
            return false;
        };
        for c in [&candidates[i], &candidates[j]] {
            if let (Some(sha), Some(fp)) = (&c.git_head_sha, meta_str(&c.meta, "file_path")) {
                if git_file_changed_since(root, sha, &fp) {
                    return true;
                }
            }
        }
        false
    };
    pairs.sort_by_key(|&(i, j)| !pair_priority(i, j));

    let mut resolutions = Vec::new();
    let mut tokens_used = 0usize;
    let mut budget_exhausted = false;

    for &(i, j) in &pairs {
        if tokens_used >= budget.max_tokens || started.elapsed() >= budget.max_duration {
            budget_exhausted = true;
            break;
        }
        let git_prio = pair_priority(i, j);
        let (res_opt, tok) = match llm {
            Some(client) => detect_conflict_online(client, &candidates[i], &candidates[j], git_prio).await,
            None => (detect_conflict_offline(&candidates[i], &candidates[j], git_prio), 0),
        };
        tokens_used += tok;
        if let Some(res) = res_opt {
            resolutions.push(res);
        }
    }

    let (edges_created, superseded_count) = if apply && !resolutions.is_empty() {
        apply_reconcile_resolutions(conn, &resolutions)?
    } else {
        (0, 0)
    };

    Ok(ReconcileReport {
        preview_only: !apply,
        candidates_checked: candidates.len(),
        pairs_after_blocking: pairs.len(),
        resolutions,
        edges_created,
        superseded_count,
        tokens_used,
        budget_exhausted,
        rolled_back: false,
    })
}

/// Send-безопасная обёртка для `Dream::run` и MCP (`tokio::spawn` без удержания `MutexGuard` через `.await`).
pub async fn reconcile_knowledge_db(
    db: &Database,
    llm: Option<&dyn LLMClient>,
    project_id: Option<&str>,
    min_importance: f64,
    apply: bool,
    require_cosine: bool,
    budget: DreamBudget,
    repo_root: Option<&Path>,
) -> anyhow::Result<ReconcileReport> {
    let started = Instant::now();
    let candidates = db.with_conn(|conn| load_active_candidates(conn, project_id, min_importance, 250))?;
    let mut pairs = filter_conflict_candidates(&candidates, require_cosine);

    let pair_priority = |i: usize, j: usize| -> bool {
        let Some(root) = repo_root else {
            return false;
        };
        for c in [&candidates[i], &candidates[j]] {
            if let (Some(sha), Some(fp)) = (&c.git_head_sha, meta_str(&c.meta, "file_path")) {
                if git_file_changed_since(root, sha, &fp) {
                    return true;
                }
            }
        }
        false
    };
    pairs.sort_by_key(|&(i, j)| !pair_priority(i, j));

    let mut resolutions = Vec::new();
    let mut tokens_used = 0usize;
    let mut budget_exhausted = false;

    for &(i, j) in &pairs {
        if tokens_used >= budget.max_tokens || started.elapsed() >= budget.max_duration {
            budget_exhausted = true;
            break;
        }
        let git_prio = pair_priority(i, j);
        let (res_opt, tok) = match llm {
            Some(client) => detect_conflict_online(client, &candidates[i], &candidates[j], git_prio).await,
            None => (detect_conflict_offline(&candidates[i], &candidates[j], git_prio), 0),
        };
        tokens_used += tok;
        if let Some(res) = res_opt {
            resolutions.push(res);
        }
    }

    let (edges_created, superseded_count) = if apply && !resolutions.is_empty() {
        db.with_conn(|conn| apply_reconcile_resolutions(conn, &resolutions))?
    } else {
        (0, 0)
    };

    Ok(ReconcileReport {
        preview_only: !apply,
        candidates_checked: candidates.len(),
        pairs_after_blocking: pairs.len(),
        resolutions,
        edges_created,
        superseded_count,
        tokens_used,
        budget_exhausted,
        rolled_back: false,
    })
}

/// Запуск сверки с автоматическим откатом SAVEPOINT при деградации поисковых метрик (74.7, ADR-13).
pub async fn reconcile_with_rollback_guard<F>(
    conn: &Connection,
    llm: Option<&dyn LLMClient>,
    project_id: Option<&str>,
    min_importance: f64,
    require_cosine: bool,
    budget: DreamBudget,
    repo_root: Option<&Path>,
    mut measure_metrics: F,
) -> anyhow::Result<ReconcileReport>
where
    F: FnMut(&Connection) -> (f64, f64),
{
    let baseline = measure_metrics(conn);
    conn.execute_batch("SAVEPOINT dream_reconcile_guard;")?;

    let mut report = match reconcile_knowledge(
        conn,
        llm,
        project_id,
        min_importance,
        true,
        require_cosine,
        budget,
        repo_root,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO SAVEPOINT dream_reconcile_guard; RELEASE SAVEPOINT dream_reconcile_guard;");
            return Err(e);
        }
    };

    let after = measure_metrics(conn);
    if is_degradation(baseline, after) {
        conn.execute_batch(
            "ROLLBACK TO SAVEPOINT dream_reconcile_guard; RELEASE SAVEPOINT dream_reconcile_guard;",
        )?;
        report.edges_created = 0;
        report.superseded_count = 0;
        report.rolled_back = true;
    } else {
        conn.execute_batch("RELEASE SAVEPOINT dream_reconcile_guard;")?;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::vector::similarity::serialize;

    fn mk_cand(
        id: i64,
        key: &str,
        content: &str,
        category: &str,
        meta: &str,
        updated_at: &str,
        emb: Option<Vec<f32>>,
    ) -> ConflictCandidate {
        ConflictCandidate {
            id,
            key: key.to_string(),
            content: content.to_string(),
            category: category.to_string(),
            meta: meta.to_string(),
            importance: 0.8,
            trust: 0.8,
            updated_at: updated_at.to_string(),
            embedding: emb,
            git_head_sha: None,
        }
    }

    #[test]
    fn test_blocking_entity_overlap_and_cosine_threshold() {
        let same_vec = vec![1.0, 0.0, 0.0, 0.0];
        let orthogonal_vec = vec![0.0, 1.0, 0.0, 0.0];
        let cands = vec![
            mk_cand(1, "k1", "таймаут Auth 5с", "fact", "{}", "2026-01-01", Some(same_vec.clone())),
            mk_cand(2, "k2", "таймаут Billing 5с", "fact", "{}", "2026-01-02", Some(same_vec.clone())),
            mk_cand(3, "k3", "таймаут Auth 10с", "fact", "{}", "2026-01-03", Some(same_vec)),
            mk_cand(4, "k4", "таймаут Auth 15с", "fact", "{}", "2026-01-04", Some(orthogonal_vec)),
        ];
        let pairs = filter_conflict_candidates(&cands, true);
        // (1, 2) отсечён из-за несовпадения сущностей Auth vs Billing;
        // (1, 4) и (3, 4) отсечены по cosine <= 0.82;
        // остаётся только (1, 3)!
        assert_eq!(pairs, vec![(0, 2)]);
    }

    #[test]
    fn test_three_level_offline_conflict_classification() {
        let a = mk_cand(1, "k1", "таймаут Auth 5с", "fact", "{}", "2026-01-01", None);
        let b = mk_cand(2, "k2", "таймаут Auth 10с", "fact", "{}", "2026-01-02", None);
        let r1 = detect_conflict_offline(&a, &b, false).unwrap();
        assert_eq!(r1.class, ConflictClass::TemporalSupersession);
        assert_eq!(r1.relation, "SUPERSEDES");

        let c = mk_cand(3, "k3", "Исправление: Auth не использует cookie, а JWT", "fact", "{}", "2026-01-03", None);
        let r2 = detect_conflict_offline(&a, &c, false).unwrap();
        assert_eq!(r2.class, ConflictClass::UserCorrection);
        assert_eq!(r2.relation, "CONTRADICTS");

        let d_dev = mk_cand(4, "k4", "в dev Auth порт 8080", "fact", "{}", "2026-01-01", None);
        let d_prod = mk_cand(5, "k5", "в prod Auth порт 443", "fact", "{}", "2026-01-02", None);
        let r3 = detect_conflict_offline(&d_dev, &d_prod, false).unwrap();
        assert_eq!(r3.class, ConflictClass::ContextualCoexistence);
        assert_eq!(r3.relation, "COEXISTS");
    }

    #[tokio::test]
    async fn test_apply_idempotency_and_rollback_guard() {
        let db = Database::in_memory().unwrap();
        let conn_arc = db.conn_arc();
        let conn = conn_arc.lock().unwrap();

        let v = serialize(&[1.0f32, 0.0, 0.0, 0.0]);
        conn.execute(
            "INSERT INTO memories (id, key, content, category, importance, trust, embedding, created_at, updated_at)
             VALUES (1, 'auth_t1', 'таймаут Auth 5с', 'fact', 0.9, 0.9, ?1, '2026-01-01', '2026-01-01'),
                    (2, 'auth_t2', 'таймаут Auth 10с', 'fact', 0.9, 0.9, ?1, '2026-01-02', '2026-01-02')",
            params![v],
        )
        .unwrap();

        // 1. Preview не меняет БД
        let preview = reconcile_knowledge(
            &conn,
            None,
            None,
            0.5,
            false,
            true,
            DreamBudget::default(),
            None,
        )
        .await
        .unwrap();
        assert!(preview.preview_only);
        assert_eq!(preview.resolutions.len(), 1);
        assert_eq!(preview.edges_created, 0);

        // 2. Rollback guard откатывает при деградации метрик
        let mut step = 0;
        let rolled = reconcile_with_rollback_guard(
            &conn,
            None,
            None,
            0.5,
            true,
            DreamBudget::default(),
            None,
            |_| {
                step += 1;
                if step == 1 { (0.90, 0.80) } else { (0.50, 0.40) }
            },
        )
        .await
        .unwrap();
        assert!(rolled.rolled_back);
        let status_after_rb: String = conn
            .query_row("SELECT status FROM memories WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status_after_rb, "ACTIVE");

        // 3. Нормальное применение + идемпотентность повторного вызова
        let applied = reconcile_knowledge(
            &conn,
            None,
            None,
            0.5,
            true,
            true,
            DreamBudget::default(),
            None,
        )
        .await
        .unwrap();
        assert!(!applied.preview_only);
        assert_eq!(applied.edges_created, 1);
        assert_eq!(applied.superseded_count, 1);

        let status1: String = conn
            .query_row("SELECT status FROM memories WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status1, "SUPERSEDED");

        let second = reconcile_knowledge(
            &conn,
            None,
            None,
            0.5,
            true,
            true,
            DreamBudget::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(second.edges_created, 0);
        assert_eq!(second.superseded_count, 0);
    }
}
