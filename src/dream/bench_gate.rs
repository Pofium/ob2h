//! Ф30 (PLAN_v1.4): ночной bench-гейт дрима.
//!
//! После успешного дрима AutoDreamWorker прогоняет quick-набор golden set
//! (первые 15 кейсов, mode=context) и сравнивает с `bench:last` (kv):
//! относительное падение recall@5 > 10% ИЛИ MRR > 15% → dream_restore на
//! предыдущий workspace-коммит + алерт в дрим-отчёт. Timeout ≠ rollback:
//! не уложился в бюджет — warning, гейт пропущен, `bench:last` не трогается
//! (никаких решений по неполным данным). Гейт выключен по умолчанию
//! (`OB2H_BENCH_GATE=1` — включить).
//!
//! Дополнение к плану (реализационная заметка): bench по build_context видит
//! только таблицу memories, а дрим правит ещё и MD-файлы workspace. Чтобы
//! тест Ф30 «дрим, портящий MEMORY.md → rollback» был честным, гейт перед
//! прогоном проверяет workspace-инвариант: обвал tracked-файла (>50% строк
//! относительно коммита до дрима) приравнивается к деградации.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tracing::warn;

use crate::cli::bench::{load_golden, run_bench, GoldenCase};
use crate::config::Settings;
use crate::db::Database;
use crate::memory::MemoryService;
use crate::workspace::GitStore;

/// Размер quick-набора: первые N кейсов golden set (Ф30.1).
pub const QUICK_CASES: usize = 15;
/// Порог деградации recall@5 — относительное падение vs bench:last (Ф30.2).
pub const RECALL_DROP_LIMIT: f64 = 0.10;
/// Порог деградации MRR — относительное падение vs bench:last (Ф30.2).
pub const MRR_DROP_LIMIT: f64 = 0.15;
/// Workspace-инвариант: обвал tracked-файла больше этой доли строк = испорчен.
pub const MD_LINE_DROP_LIMIT: f64 = 0.50;

/// Итог ночного гейта — попадает в stats dream_runs (виден в dream_status)
/// и в kv `bench:last_gate`.
#[derive(Debug, Clone)]
pub struct GateVerdict {
    /// pass | first_run | rollback | timeout | skipped
    pub outcome: String,
    pub reason: Option<String>,
    pub cases: usize,
    pub recall5: Option<f64>,
    pub recall10: Option<f64>,
    pub mrr: Option<f64>,
    pub p95_ms: Option<f64>,
    pub prev_recall5: Option<f64>,
    pub prev_mrr: Option<f64>,
    pub elapsed_ms: u64,
    pub restored_to: Option<String>,
    pub alert: Option<String>,
}

impl GateVerdict {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "outcome": self.outcome,
            "reason": self.reason,
            "cases": self.cases,
            "recall5": self.recall5,
            "recall10": self.recall10,
            "mrr": self.mrr,
            "p95_ms": self.p95_ms,
            "prev_recall5": self.prev_recall5,
            "prev_mrr": self.prev_mrr,
            "elapsed_ms": self.elapsed_ms,
            "restored_to": self.restored_to,
            "alert": self.alert,
        })
    }
}

/// Деградация: относительное падение любого из двух сигналов сверх порога
/// (Ф30.2: «падение только MRR при стабильном recall — тоже деградация»).
pub fn is_degradation(prev: (f64, f64), cur: (f64, f64)) -> bool {
    let (prev_recall, prev_mrr) = prev;
    let (cur_recall, cur_mrr) = cur;
    let recall_drop =
        prev_recall > f64::EPSILON && (prev_recall - cur_recall) / prev_recall > RECALL_DROP_LIMIT;
    let mrr_drop = prev_mrr > f64::EPSILON && (prev_mrr - cur_mrr) / prev_mrr > MRR_DROP_LIMIT;
    recall_drop || mrr_drop
}

fn history_path(settings: &Settings) -> PathBuf {
    settings.data_dir.join("bench").join("history.jsonl")
}

/// Последние `last` строк истории прогонов (Ф30.4, `ob2h bench history`).
pub fn read_history(settings: &Settings, last: usize) -> anyhow::Result<Vec<serde_json::Value>> {
    let path = history_path(settings);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            rows.push(v);
        }
    }
    let start = rows.len().saturating_sub(last);
    Ok(rows.split_off(start))
}

fn append_history(settings: &Settings, row: &serde_json::Value) {
    let path = history_path(settings);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{row}");
    }
}

/// Значение метрики из kv bench:last (`{"recall5":…, "mrr":…}`).
fn read_bench_last(db: &Database) -> Option<(f64, f64)> {
    let raw = db.get_kv("bench:last").ok()??;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((v.get("recall5")?.as_f64()?, v.get("mrr")?.as_f64()?))
}

/// Строка истории по схеме §3 плана (+аддитивное поле gate).
#[allow(clippy::too_many_arguments)]
fn history_row(
    gate: &str,
    recall5: Option<f64>,
    recall10: Option<f64>,
    mrr: Option<f64>,
    p95_ms: Option<f64>,
    dream_sha: Option<&str>,
    settings: &Settings,
) -> serde_json::Value {
    let db_size_mb = settings
        .db_path()
        .metadata()
        .map(|m| m.len() as f64 / 1e6)
        .unwrap_or(0.0);
    json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "mode": "context",
        "gate": gate,
        "recall@5": recall5,
        "recall@10": recall10,
        "mrr": mrr,
        "p95_ms": p95_ms.map(|v| (v * 10.0).round() / 10.0),
        "db_size_mb": (db_size_mb * 10.0).round() / 10.0,
        "embedding_backend": settings.embed_provider,
        "dream_sha": dream_sha,
    })
}

/// Ночной bench-гейт. Вызывается воркером после успешного дрима (status=ok).
/// `prev_sha` — HEAD workspace до дрима (точка отката). Возвращает None,
/// если гейт выключен.
pub async fn run_gate(
    memory: &Arc<MemoryService>,
    db: &Database,
    gitstore: &Arc<GitStore>,
    settings: &Settings,
    dream_sha: Option<&str>,
    prev_sha: Option<&str>,
) -> Option<GateVerdict> {
    if !settings.bench_gate_enabled {
        return None;
    }
    let started = Instant::now();

    // 30.3: без golden set гейт пропускается с warning (не с ошибкой).
    let golden_path = settings.data_dir.join("bench").join("golden.jsonl");
    if !golden_path.is_file() {
        return Some(skip_verdict(
            started,
            "golden-набор отсутствует — гейт пропущен (создайте data/bench/golden.jsonl)",
        ));
    }
    let cases: Vec<GoldenCase> = match load_golden(&golden_path) {
        Ok(c) => c,
        Err(e) => return Some(skip_verdict(started, &format!("golden-набор не читается: {e}"))),
    };
    if cases.is_empty() {
        return Some(skip_verdict(started, "golden-набор пуст — гейт пропущен"));
    }
    let quick: Vec<GoldenCase> = cases.into_iter().take(QUICK_CASES).collect();

    // Workspace-инвариант: испорченный MD-файл bench по БД не увидит.
    if let Some(prev) = prev_sha {
        if let Some(file) = gitstore.file_shrunk_beyond(prev, MD_LINE_DROP_LIMIT) {
            let restored = gitstore.restore(prev);
            let alert = format!(
                "ОТКАТ: дрим {} обрушил {file} (>50% строк) — workspace восстановлен ({restored})",
                dream_sha.unwrap_or("?"),
            );
            append_history(
                settings,
                &history_row("rollback", None, None, None, None, dream_sha, settings),
            );
            bump_runs(db);
            return Some(GateVerdict {
                outcome: "rollback".to_string(),
                reason: Some(format!("workspace-инвариант: {file}")),
                cases: 0,
                recall5: None,
                recall10: None,
                mrr: None,
                p95_ms: None,
                prev_recall5: None,
                prev_mrr: None,
                elapsed_ms: ms_since(started),
                restored_to: Some(prev.to_string()),
                alert: Some(alert),
            });
        }
    }

    // 30.1: quick-набор, mode=context, бюджет OB2H_BENCH_GATE_TIMEOUT_MS.
    let ks = vec![5usize, 10usize];
    let bench_fut = run_bench(memory, &quick, "context", &ks);
    let result = match tokio::time::timeout(Duration::from_millis(settings.bench_gate_timeout_ms), bench_fut)
        .await
    {
        // 30.2: timeout ≠ rollback — warning, bench:last не трогается.
        Err(_) => {
            warn!(
                "bench-гейт: таймаут {} мс — гейт пропущен БЕЗ отката (решений по неполным данным нет)",
                settings.bench_gate_timeout_ms
            );
            append_history(
                settings,
                &history_row("timeout", None, None, None, None, dream_sha, settings),
            );
            bump_runs(db);
            return Some(GateVerdict {
                outcome: "timeout".to_string(),
                reason: Some(format!("бюджет {} мс исчерпан", settings.bench_gate_timeout_ms)),
                cases: quick.len(),
                recall5: None,
                recall10: None,
                mrr: None,
                p95_ms: None,
                prev_recall5: None,
                prev_mrr: None,
                elapsed_ms: ms_since(started),
                restored_to: None,
                alert: None,
            });
        }
        Ok(Err(e)) => {
            return Some(skip_verdict(started, &format!("bench не выполнился: {e}")))
        }
        Ok(Ok(r)) => r,
    };

    let recall5 = result.recall.first().copied().unwrap_or(0.0);
    let recall10 = result.recall.get(1).copied().unwrap_or(0.0);
    let mrr = result.mrr;

    let prev_metrics = read_bench_last(db);
    let (outcome, alert, restored_to) = match prev_metrics {
        None => ("first_run".to_string(), None, None),
        Some(prev) => {
            if is_degradation(prev, (recall5, mrr)) {
                // 30.2: откат на предыдущий workspace-коммит; bench:last не обновляется.
                let restore_target = prev_sha.map(|s| s.to_string());
                if let Some(target) = &restore_target {
                    gitstore.restore(target);
                }
                let alert = format!(
                    "ОТКАТ: дрим {} ухудшил recall@5 с {:.3} до {:.3} / MRR с {:.3} до {:.3}",
                    dream_sha.unwrap_or("?"),
                    prev.0,
                    recall5,
                    prev.1,
                    mrr
                );
                warn!("{alert}");
                ("rollback".to_string(), Some(alert), restore_target)
            } else {
                ("pass".to_string(), None, None)
            }
        }
    };

    // Здоровый путь: обновление bench:last.
    if outcome == "pass" || outcome == "first_run" {
        let _ = db.set_kv(
            "bench:last",
            &json!({
                "recall5": recall5,
                "mrr": mrr,
                "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })
            .to_string(),
        );
    }

    append_history(
        settings,
        &history_row(
            &outcome,
            Some(recall5),
            Some(recall10),
            Some(mrr),
            Some(result.p95_ms),
            dream_sha,
            settings,
        ),
    );
    bump_runs(db);

    Some(GateVerdict {
        reason: None,
        cases: result.cases,
        recall5: Some(recall5),
        recall10: Some(recall10),
        mrr: Some(mrr),
        p95_ms: Some(result.p95_ms),
        prev_recall5: prev_metrics.map(|p| p.0),
        prev_mrr: prev_metrics.map(|p| p.1),
        outcome,
        elapsed_ms: ms_since(started),
        restored_to,
        alert,
    })
}

/// Счётчик прогонов гейта — для решения по реранкеру в Ф35.2 (≥ 30 прогонов).
fn bump_runs(db: &Database) {
    let cur: i64 = db
        .get_kv("bench:runs")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let _ = db.set_kv("bench:runs", &(cur + 1).to_string());
}

fn skip_verdict(started: Instant, reason: &str) -> GateVerdict {
    warn!("bench-гейт: {reason}");
    GateVerdict {
        outcome: "skipped".to_string(),
        reason: Some(reason.to_string()),
        cases: 0,
        recall5: None,
        recall10: None,
        mrr: None,
        p95_ms: None,
        prev_recall5: None,
        prev_mrr: None,
        elapsed_ms: ms_since(started),
        restored_to: None,
        alert: None,
    }
}

fn ms_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
