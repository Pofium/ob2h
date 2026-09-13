//! `ob2h bench` — регрессионный контур retrieval (Фаза 21, PLAN_v1.3.md).
//! Прогон golden-набора через `memory_search` (гибрид) или `memory_context`
//! (build_context) с метриками recall@k, MRR, долей пустых выдач и латентностью.
//! Golden set — `data/bench/golden.jsonl`: персональные данные, в git не входит
//! (в репо — синтетический `tests/fixtures/golden_synthetic.jsonl`).

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use crate::memory::MemoryService;

/// Одна строка golden-набора: запрос → ключи, которые обязан найтись.
#[derive(Debug, Deserialize)]
pub struct GoldenCase {
    pub query: String,
    pub expect_keys: Vec<String>,
    #[serde(default)]
    pub note: String,
}

/// Загрузить golden-набор (JSONL, пустые строки пропускаются).
pub fn load_golden(path: &Path) -> anyhow::Result<Vec<GoldenCase>> {
    let text = std::fs::read_to_string(path)?;
    let mut cases = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let case: GoldenCase = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("golden-строка {}: {e} — {line}", i + 1))?;
        if case.query.trim().is_empty() {
            anyhow::bail!("golden-строка {}: пустой query", i + 1);
        }
        if case.expect_keys.is_empty() {
            anyhow::bail!("golden-строка {}: expect_keys пуст", i + 1);
        }
        cases.push(case);
    }
    Ok(cases)
}

/// recall@k: доля ожидаемых ключей, попавших в топ-k ранжированной выдачи.
pub fn recall_at_k(ranked: &[String], expected: &HashSet<String>, k: usize) -> f64 {
    if expected.is_empty() {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|key| expected.contains(*key))
        .count();
    hits as f64 / expected.len() as f64
}

/// MRR: 1/позиция первого ожидаемого ключа (0, если не нашёлся).
pub fn mrr(ranked: &[String], expected: &HashSet<String>) -> f64 {
    for (i, key) in ranked.iter().enumerate() {
        if expected.contains(key) {
            return 1.0 / (i + 1) as f64;
        }
    }
    0.0
}

/// Перцентиль методом nearest-rank (p в процентах: 50, 95).
pub fn percentile(durations_ms: &mut [f64], p: f64) -> f64 {
    if durations_ms.is_empty() {
        return 0.0;
    }
    durations_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((p / 100.0) * durations_ms.len() as f64).ceil() as usize;
    let idx = idx.clamp(1, durations_ms.len());
    durations_ms[idx - 1]
}

/// Нормализация для context-матчинга: нижний регистр + схлопывание пробелов.
pub fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Агрегированный результат прогона.
pub struct BenchResult {
    pub mode: String,
    pub cases: usize,
    pub ks: Vec<usize>,
    pub recall: Vec<f64>,
    pub mrr: f64,
    pub empty_count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub missing_keys: Vec<String>,
}

/// Прогон кейсов. `search` — гибридный memory_search (как агент вызывает по умолчанию),
/// `context` — build_context (автоподстановка prefetch) с матчингом по содержимому записей.
pub async fn run_bench(
    memory: &MemoryService,
    cases: &[GoldenCase],
    mode: &str,
    ks: &[usize],
) -> anyhow::Result<BenchResult> {
    if ks.is_empty() {
        anyhow::bail!("список k пуст");
    }
    let max_k = *ks.iter().max().unwrap();

    // Ключи, отсутствующие в БД, исключаем из ожиданий (и сообщаем — гигиена golden set).
    let mut needles: std::collections::HashMap<String, String> = Default::default();
    let mut missing_keys = Vec::new();
    for case in cases {
        for key in &case.expect_keys {
            if needles.contains_key(key) || missing_keys.contains(key) {
                continue;
            }
            match memory.get(key)? {
                Some(rec) => {
                    let n = normalize(&rec.content);
                    let needle: String = n.chars().take(120).collect();
                    needles.insert(key.clone(), needle);
                }
                None => missing_keys.push(key.clone()),
            }
        }
    }

    // Прогрев: первый вызов грузит локальную модель эмбеддингов — не включаем его в метрики.
    if mode == "context" {
        let _ = memory.build_context(20, Some("прогрев"));
    } else {
        let _ = memory.search_hybrid("прогрев", 5, 0.0).await?;
    }

    let mut recalls = vec![0.0f64; ks.len()];
    let mut mrr_sum = 0.0f64;
    let mut empty_count = 0usize;
    let mut durations = Vec::with_capacity(cases.len());
    let mut scored_cases = 0usize;

    for case in cases {
        let expected: HashSet<String> = case
            .expect_keys
            .iter()
            .filter(|k| needles.contains_key(*k))
            .cloned()
            .collect();
        if expected.is_empty() {
            continue;
        }
        scored_cases += 1;

        let started = Instant::now();
        let ranked: Vec<String> = if mode == "context" {
            let block = memory.build_context(20, Some(&case.query))?;
            let mut ranked = Vec::new();
            for line in block.lines() {
                let nl = normalize(line);
                for key in &case.expect_keys {
                    if !ranked.contains(key) && needles.get(key).is_some_and(|n| nl.contains(n)) {
                        ranked.push(key.clone());
                    }
                }
            }
            ranked
        } else {
            memory
                .search_hybrid(&case.query, max_k, 0.0)
                .await?
                .into_iter()
                .map(|h| h.record.key)
                .collect()
        };
        durations.push(started.elapsed().as_secs_f64() * 1000.0);

        if ranked.is_empty() {
            empty_count += 1;
        }
        for (i, k) in ks.iter().enumerate() {
            recalls[i] += recall_at_k(&ranked, &expected, *k);
        }
        mrr_sum += mrr(&ranked, &expected);
    }

    let n = scored_cases.max(1) as f64;
    for r in &mut recalls {
        *r /= n;
    }

    Ok(BenchResult {
        mode: mode.to_string(),
        cases: scored_cases,
        ks: ks.to_vec(),
        recall: recalls,
        mrr: mrr_sum / n,
        empty_count,
        p50_ms: percentile(&mut durations, 50.0),
        p95_ms: percentile(&mut durations, 95.0),
        missing_keys,
    })
}

/// Полный прогон CLI: печать таблицы, --json, --save-baseline.
pub async fn cli_run(
    ctx: &crate::mcp::AppContext,
    mode: &str,
    k_spec: &str,
    golden: Option<&str>,
    as_json: bool,
    save_baseline: bool,
) -> anyhow::Result<()> {
    if mode != "search" && mode != "context" {
        anyhow::bail!("mode должен быть search|context, получено: {mode}");
    }
    let ks: Vec<usize> = k_spec
        .split(',')
        .map(|s| s.trim().parse::<usize>())
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("--k: ожидались числа через запятую (напр. 5,10): {e}"))?;
    if ks.contains(&0) {
        anyhow::bail!("--k: уровни k должны быть ≥ 1");
    }

    let golden_path: std::path::PathBuf = match golden {
        Some(p) => std::path::PathBuf::from(p),
        None => ctx.settings.data_dir.join("bench").join("golden.jsonl"),
    };
    if !golden_path.is_file() {
        anyhow::bail!(
            "golden-набор не найден: {}\nСоздайте data/bench/golden.jsonl ({{query, expect_keys, note}} per line)",
            golden_path.display()
        );
    }
    let cases = load_golden(&golden_path)?;
    if cases.is_empty() {
        anyhow::bail!("golden-набор пуст: {}", golden_path.display());
    }

    let memory = ctx.memory.clone();
    let mode_s = mode.to_string();
    let ks_clone = ks.clone();
    let result = run_bench(&memory, &cases, &mode_s, &ks_clone).await?;

    if as_json {
        println!(
            "{}",
            json!({
                "mode": result.mode,
                "cases": result.cases,
                "k": result.ks,
                "recall_at_k": result.recall,
                "mrr": result.mrr,
                "empty_count": result.empty_count,
                "p50_ms": (result.p50_ms * 10.0).round() / 10.0,
                "p95_ms": (result.p95_ms * 10.0).round() / 10.0,
                "missing_keys": result.missing_keys,
            })
        );
    } else {
        print_human(&result, &golden_path);
    }

    if save_baseline {
        let out = std::path::PathBuf::from("docs").join("bench_baseline.md");
        write_baseline(&result, &out)?;
        println!("baseline сохранён: {}", out.display());
    }
    Ok(())
}

fn print_human(result: &BenchResult, golden_path: &Path) {
    println!(
        "ob2h bench v{} — golden: {}",
        env!("CARGO_PKG_VERSION"),
        golden_path.display()
    );
    println!(
        "режим: {} | кейсов: {} | ключей нет в БД: {}",
        result.mode,
        result.cases,
        result.missing_keys.len()
    );
    if !result.missing_keys.is_empty() {
        println!("  отсутствующие ключи: {}", result.missing_keys.join(", "));
    }
    println!();
    for (k, r) in result.ks.iter().zip(&result.recall) {
        println!("  recall@{k:<3} {r:.3}");
    }
    println!("  MRR        {:.3}", result.mrr);
    println!(
        "  пусто      {}/{}",
        result.empty_count, result.cases
    );
    println!("  p50        {:.1} мс", result.p50_ms);
    println!("  p95        {:.1} мс", result.p95_ms);
    println!();
    println!("Markdown для CHANGELOG.md:");
    let heads: Vec<String> = result.ks.iter().map(|k| format!("recall@{k}")).collect();
    let cells: Vec<String> = result.recall.iter().map(|r| format!("{r:.3}")).collect();
    println!("| {} | MRR | p50, мс | p95, мс |", heads.join(" | "));
    println!("|---|---|---|---|");
    println!(
        "| {} | {:.3} | {:.1} | {:.1} |",
        cells.join(" | "),
        result.mrr,
        result.p50_ms,
        result.p95_ms
    );
}

/// Baseline — только агрегаты; персональные запросы/ключи не публикуются (§9 AGENTS.md).
fn write_baseline(result: &BenchResult, out: &Path) -> anyhow::Result<()> {
    let heads: Vec<String> = result.ks.iter().map(|k| format!("recall@{k}")).collect();
    let cells: Vec<String> = result.recall.iter().map(|r| format!("{r:.3}")).collect();
    let mut md = String::new();
    md.push_str("# OB2H bench baseline (Фаза 21, PLAN_v1.3)\n\n");
    md.push_str(&format!("- Дата: {}\n", chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")));
    md.push_str(&format!("- Версия ob2h: {}\n", env!("CARGO_PKG_VERSION")));
    md.push_str(&format!("- Режим: {}\n", result.mode));
    md.push_str(&format!("- Кейсов: {}\n", result.cases));
    md.push_str("- Golden: `data/bench/golden.jsonl` (персональные данные, в git не входит;\n");
    md.push_str("  здесь — только агрегаты)\n\n");
    md.push_str(&format!("| {} | MRR | p50, мс | p95, мс |\n", heads.join(" | ")));
    md.push_str("|---|---|---|---|\n");
    md.push_str(&format!(
        "| {} | {:.3} | {:.1} | {:.1} |\n",
        cells.join(" | "),
        result.mrr,
        result.p50_ms,
        result.p95_ms
    ));
    md.push_str("\n> Гейт (ADR-13): изменения формул релевантности/квантования принимаются,\n");
    md.push_str("> пока recall@k не ниже baseline − 2 п.п.\n");
    std::fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(out, md)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recall_hits_partial_and_miss() {
        let expected = set(&["a", "b", "c"]);
        let ranked: Vec<String> = vec!["x", "a", "y", "b"].into_iter().map(String::from).collect();
        assert_eq!(recall_at_k(&ranked, &expected, 1), 0.0);
        assert!((recall_at_k(&ranked, &expected, 2) - 1.0 / 3.0).abs() < 1e-9);
        assert!((recall_at_k(&ranked, &expected, 4) - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(recall_at_k(&ranked, &expected, 100), 2.0 / 3.0);
        assert_eq!(recall_at_k(&[], &expected, 5), 0.0);
        assert_eq!(recall_at_k(&ranked, &set(&[]), 5), 0.0);
    }

    #[test]
    fn mrr_positions() {
        let expected = set(&["b"]);
        let ranked: Vec<String> = vec!["a", "b", "c"].into_iter().map(String::from).collect();
        assert!((mrr(&ranked, &expected) - 0.5).abs() < 1e-9);
        assert_eq!(mrr(&ranked, &set(&["z"])), 0.0);
        let top: Vec<String> = vec!["b"].into_iter().map(String::from).collect();
        assert!((mrr(&top, &expected) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn percentile_nearest_rank() {
        let mut d = vec![30.0, 10.0, 20.0];
        assert_eq!(percentile(&mut d, 50.0), 20.0);
        let mut d2 = vec![10.0, 20.0, 30.0, 40.0];
        assert_eq!(percentile(&mut d2, 95.0), 40.0);
        let mut empty: Vec<f64> = vec![];
        assert_eq!(percentile(&mut empty, 50.0), 0.0);
    }

    #[test]
    fn normalize_collapses_whitespace_and_case() {
        assert_eq!(normalize("  Привет   МИР \n"), "привет мир");
    }

    #[test]
    fn load_golden_parses_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("golden.jsonl");
        std::fs::write(
            &path,
            "{\"query\":\"кофе\",\"expect_keys\":[\"k1\"],\"note\":\"n\"}\n\n\
             {\"query\":\"rust\",\"expect_keys\":[\"k2\",\"k3\"]}\n",
        )
        .unwrap();
        let cases = load_golden(&path).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[1].expect_keys, vec!["k2", "k3"]);

        let bad = dir.path().join("bad.jsonl");
        std::fs::write(&bad, "{not json}\n").unwrap();
        assert!(load_golden(&bad).is_err());

        let empty_q = dir.path().join("empty_q.jsonl");
        std::fs::write(&empty_q, "{\"query\":\"  \",\"expect_keys\":[\"k1\"]}\n").unwrap();
        assert!(load_golden(&empty_q).is_err());

        let no_keys = dir.path().join("no_keys.jsonl");
        std::fs::write(&no_keys, "{\"query\":\"x\",\"expect_keys\":[]}\n").unwrap();
        assert!(load_golden(&no_keys).is_err());
    }
}
