//! Ф35.2 / 30.3 (PLAN_v1.4): накопительная статистика bench.
//!
//! Каждый прогон bench дописывает строку в `data/bench/history.jsonl`
//! (§4: `{ts, mode, recall@5, recall@10, mrr, p95_ms, db_size_mb,
//! embedding_backend, dream_sha, ...}` — бэкенд эмбеддингов и размер БД
//! обязательны: ночные регрессии от подмены модели видны в тренде).
//!
//! `ob2h bench --mode history` — сводка: счётчики по режимам, тренд p95
//! (первые 10 vs последние 10), сравнение OB2H_VEC0 on/off и вердикт 35.2
//! по реранкеру (честно: без прогнанного rerank-профиля решение не принимается).
//!
//! Ночной прогон: сервер (serve) раз в 24 ч запускает latency-бенч по
//! golden-набору и дописывает history (best-effort, fail-open);
//! `OB2H_BENCH_NIGHTLY=0` выключает.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

/// Путь к history.jsonl в data-каталоге.
pub fn history_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bench").join("history.jsonl")
}

/// Дописать запись прогона в history.jsonl (best-effort: ошибка не роняет bench).
pub fn append_record(path: &Path, record: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{record}")
}

/// Модулярность не нужна здесь; сводка по history.jsonl.
#[derive(Debug, Default, serde::Serialize)]
pub struct HistorySummary {
    pub total_runs: usize,
    pub by_mode: std::collections::BTreeMap<String, usize>,
    pub latency_runs: usize,
    pub latency_off_p95_median: Option<f64>,
    pub latency_on_p95_median: Option<f64>,
    pub last_ts: Option<String>,
    pub verdict: String,
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

/// Прочитать и просуммировать history.jsonl (битые строки пропускаются).
pub fn analyze(path: &Path) -> Result<HistorySummary> {
    let mut summary = HistorySummary::default();
    if !path.is_file() {
        summary.verdict =
            "history.jsonl отсутствует — ночные прогоны ещё не запускались".to_string();
        return Ok(summary);
    }
    let text = std::fs::read_to_string(path)?;
    let mut off_p95: Vec<f64> = Vec::new();
    let mut on_p95: Vec<f64> = Vec::new();
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        summary.total_runs += 1;
        let mode = rec
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let is_latency = mode == "latency";
        *summary.by_mode.entry(mode).or_default() += 1;
        if let Some(ts) = rec.get("ts").and_then(|v| v.as_str()) {
            summary.last_ts = Some(ts.to_string());
        }
        if is_latency {
            summary.latency_runs += 1;
            let Some(p95) = rec.get("p95_ms").and_then(|v| v.as_f64()) else {
                continue;
            };
            let vec0 = rec
                .get("vec0")
                .and_then(|v| v.as_str())
                .map(|s| s == "on")
                .unwrap_or(false);
            if vec0 {
                on_p95.push(p95);
            } else {
                off_p95.push(p95);
            }
        }
    }
    summary.latency_off_p95_median = median(&mut off_p95);
    summary.latency_on_p95_median = median(&mut on_p95);
    summary.verdict = verdict_35_2(&summary);
    Ok(summary)
}

/// Вердикт 35.2 (честно, без выдуманных данных).
fn verdict_35_2(s: &HistorySummary) -> String {
    let mut parts = Vec::new();
    if s.total_runs < 30 {
        parts.push(format!(
            "данных {}/30 ночных прогонов — решение по реранкеру (35.2) откладывается",
            s.total_runs
        ));
    } else {
        parts.push("накоплено ≥30 прогонов".to_string());
    }
    // Реранкер: сравнение профилей требует прогнанных OB2H_RERANK=1 запусков;
    // в history такие строки не пишутся, пока реранкер не реализован (25.3 — бэклог).
    parts.push(
        "rerank-профиль в history отсутствует (25.3 не реализован) — OB2H_RERANK остаётся off, решение в bench-отчёте"
            .to_string(),
    );
    match (s.latency_off_p95_median, s.latency_on_p95_median) {
        (Some(off), Some(on)) if s.latency_runs >= 10 => {
            let gain = (off - on) / off * 100.0;
            if gain >= 30.0 {
                parts.push(format!(
                    "vec0 on: медиана p95 {on:.0} мс против off {off:.0} мс (−{gain:.0}%) — данные поддерживают включение по умолчанию, решение за владельцем (ADR-35.1)"
                ));
            } else {
                parts.push(format!(
                    "vec0 on: медиана p95 {on:.0} мс против off {off:.0} мс (−{gain:.0}%) — выигрыш < 30%, остаёмся opt-in"
                ));
            }
        }
        (Some(off), None) => {
            parts.push(format!(
                "vec0 off: медиана p95 {off:.0} мс; прогонов с OB2H_VEC0=1 нет — сравнение отложено"
            ));
        }
        _ => {}
    }
    parts.join("; ")
}

/// Запись прогона latency в формате §4 (+детали).
pub fn latency_record(
    lat: &crate::cli::bench::LatencyResult,
    db_path: &Path,
    embed_backend: &str,
    vec0_on: bool,
) -> serde_json::Value {
    let db_size_mb = std::fs::metadata(db_path)
        .map(|m| m.len() as f64 / 1_048_576.0)
        .unwrap_or(0.0);
    let p95_ms = lat.mem_p95_ms.max(lat.graph_p95_ms);
    json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "mode": "latency",
        "cases": lat.cases,
        "recall@5": serde_json::Value::Null,
        "recall@10": serde_json::Value::Null,
        "mrr": serde_json::Value::Null,
        "p95_ms": (p95_ms * 10.0).round() / 10.0,
        "mem_p95_ms": (lat.mem_p95_ms * 10.0).round() / 10.0,
        "graph_p95_ms": (lat.graph_p95_ms * 10.0).round() / 10.0,
        "embed_p95_ms": (lat.embed_p95_ms * 10.0).round() / 10.0,
        "db_size_mb": (db_size_mb * 10.0).round() / 10.0,
        "embedding_backend": embed_backend,
        "vec0": if vec0_on { "on" } else { "off" },
        "dream_sha": last_dream_marker(db_path),
    })
}

/// Последний dream-прогон как маркер состояния (best-effort).
pub fn last_dream_marker(db_path: &Path) -> String {
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return "none".to_string();
    };
    conn.query_row("SELECT MAX(id) FROM dream_runs", [], |r| {
        r.get::<_, Option<i64>>(0)
    })
    .map(|v| match v {
        Some(id) => format!("dream:{id}"),
        None => "no-dreams".to_string(),
    })
    .unwrap_or_else(|_| "none".to_string())
}

/// Ночной прогон (Ф30.3/35.2): раз в 24 ч — latency-бенч по golden-набору,
/// результат дописывается в history.jsonl. Best-effort, fail-open.
pub async fn nightly_loop(ctx: std::sync::Arc<crate::mcp::AppContext>) {
    if std::env::var("OB2H_BENCH_NIGHTLY").as_deref() == Ok("0") {
        tracing::info!("Nightly bench отключён (OB2H_BENCH_NIGHTLY=0)");
        return;
    }
    let golden = ctx.settings.data_dir.join("bench").join("golden.jsonl");
    if !golden.is_file() {
        tracing::info!(
            "Nightly bench: golden-набор {} отсутствует — пропущено",
            golden.display()
        );
        return;
    }
    // первый прогон через 1 ч (не мешать старту), далее каждые 24 ч
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    loop {
        let ctx2 = ctx.clone();
        let golden2 = golden.clone();
        let run = tokio::task::spawn(async move {
            match crate::cli::bench::load_golden(&golden2) {
                Ok(cases) if !cases.is_empty() => {
                    let lat = crate::cli::bench::run_latency(
                        &ctx2.memory,
                        &ctx2.graph,
                        &ctx2.embedder,
                        &ctx2.db,
                        &cases,
                    )
                    .await?;
                    let db_path = ctx2.settings.data_dir.join("ob2h.db");
                    let rec = latency_record(
                        &lat,
                        &db_path,
                        &ctx2.settings.embed_provider,
                        crate::vector::vec0::enabled(),
                    );
                    let path = history_path(&ctx2.settings.data_dir);
                    append_record(&path, &rec)?;
                    tracing::info!(
                        "Nightly bench: latency записан в {} (p95 max {:.0} мс)",
                        path.display(),
                        rec["p95_ms"].as_f64().unwrap_or(0.0)
                    );
                    Ok::<(), anyhow::Error>(())
                }
                _ => Ok(()),
            }
        });
        match run.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("Nightly bench не удался: {e}"),
            Err(e) => tracing::warn!("Nightly bench task panic: {e}"),
        }
        tick.tick().await;
    }
}

/// Печать сводки history (`ob2h bench --mode history`).
pub fn print_summary(s: &HistorySummary) {
    println!("Bench history (Ф35.2/30.3):");
    println!("  прогонов всего: {}", s.total_runs);
    for (mode, n) in &s.by_mode {
        println!("    {mode}: {n}");
    }
    if let Some(off) = s.latency_off_p95_median {
        println!("  latency p95 медиана off: {off:.1} мс");
    }
    if let Some(on) = s.latency_on_p95_median {
        println!("  latency p95 медиана on (OB2H_VEC0=1): {on:.1} мс");
    }
    if let Some(ts) = &s.last_ts {
        println!("  последний прогон: {ts}");
    }
    println!("  вердикт 35.2: {}", s.verdict);
}
