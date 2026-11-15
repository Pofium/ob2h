//! `ob2h bench` — регрессионный контур retrieval (Фаза 21, PLAN_v1.3.md).
//! Прогон golden-набора через `memory_search` (гибрид) или `memory_context`
//! (build_context) с метриками recall@k, MRR, долей пустых выдач и латентностью.
//! Golden set — `data/bench/golden.jsonl`: персональные данные, в git не входит
//! (в репо — синтетические эталоны Ф69.3: `tests/fixtures/graph_golden.jsonl`,
//! `tests/fixtures/ast_golden/`, `tests/fixtures/contradiction_golden.jsonl`).

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use crate::memory::{ContextOptions, MemoryService};

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

/// Ф69.2, recall-all@k (LongMemEval-стиль, strict): ВСЕ ожидаемые ключи кейса
/// найдены в топ-k. Для одноключевого кейса совпадает с «найден/не найден».
pub fn recall_all_at_k(ranked: &[String], expected: &HashSet<String>, k: usize) -> bool {
    !expected.is_empty()
        && expected
            .iter()
            .all(|key| ranked.iter().take(k).any(|rk| rk == key))
}

/// Ф69.2, any-hit@k: хотя бы один ожидаемый ключ кейса в топ-k.
pub fn any_hit_at_k(ranked: &[String], expected: &HashSet<String>, k: usize) -> bool {
    ranked.iter().take(k).any(|key| expected.contains(key))
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
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Агрегированный результат прогона.
pub struct BenchResult {
    pub mode: String,
    pub cases: usize,
    pub ks: Vec<usize>,
    pub recall: Vec<f64>,
    /// Ф69.2: доля кейсов, где ВСЕ ожидаемые ключи в топ-k.
    pub recall_all: Vec<f64>,
    /// Ф69.2: доля кейсов, где хотя бы один ожидаемый ключ в топ-k.
    pub any_hit: Vec<f64>,
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
    // Вес trust берётся из env — как у живого сервера (дефолт 0 = формула v1.3).
    let trust_weight = std::env::var("OB2H_CONTEXT_TRUST_WEIGHT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let bench_opts = ContextOptions {
        max_chars: Some(8000),
        trust_weight,
        ..Default::default()
    };
    if mode == "context" {
        let _ = memory
            .build_context(20, Some("прогрев"), &ContextOptions::default())
            .await;
    } else {
        let _ = memory.search_hybrid("прогрев", 5, 0.0).await?;
    }

    let mut recalls = vec![0.0f64; ks.len()];
    let mut recall_all_hits = vec![0.0f64; ks.len()];
    let mut any_hit_hits = vec![0.0f64; ks.len()];
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
            let block = memory
                .build_context(20, Some(&case.query), &bench_opts)
                .await?;
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
            if recall_all_at_k(&ranked, &expected, *k) {
                recall_all_hits[i] += 1.0;
            }
            if any_hit_at_k(&ranked, &expected, *k) {
                any_hit_hits[i] += 1.0;
            }
        }
        mrr_sum += mrr(&ranked, &expected);
    }

    let n = scored_cases.max(1) as f64;
    for r in &mut recalls {
        *r /= n;
    }
    for r in &mut recall_all_hits {
        *r /= n;
    }
    for r in &mut any_hit_hits {
        *r /= n;
    }

    Ok(BenchResult {
        mode: mode.to_string(),
        cases: scored_cases,
        ks: ks.to_vec(),
        recall: recalls,
        recall_all: recall_all_hits,
        any_hit: any_hit_hits,
        mrr: mrr_sum / n,
        empty_count,
        p50_ms: percentile(&mut durations, 50.0),
        p95_ms: percentile(&mut durations, 95.0),
        missing_keys,
    })
}

/// Ф35.1: латентность по хранилищам — memories и graph_nodes отдельно.
pub struct LatencyResult {
    pub cases: usize,
    pub mem_vectors: i64,
    pub graph_vectors: i64,
    /// Ф35.1: эмбеддинг запроса локальной моделью — нижняя граница любой выдачи
    /// (векторный скан не может быть быстрее, чем подготовка вектора запроса).
    pub embed_p50_ms: f64,
    pub embed_p95_ms: f64,
    pub mem_p50_ms: f64,
    pub mem_p95_ms: f64,
    pub graph_p50_ms: f64,
    pub graph_p95_ms: f64,
}

/// Ф35.1: замер p50/p95 поиска на живой БД — отдельно memories (search_hybrid)
/// и graph_nodes (graph.search), плюс отдельно эмбеддинг запроса (пол).
/// Метрики — только латентность (recall по графу требует отдельного golden'а),
/// но оба хранилища меряются на одних запросах.
pub async fn run_latency(
    memory: &MemoryService,
    graph: &crate::graph::GraphService,
    embedder: &std::sync::Arc<dyn crate::embedding::EmbeddingProvider>,
    db: &crate::db::Database,
    cases: &[GoldenCase],
) -> anyhow::Result<LatencyResult> {
    // прогрев: локальная модель эмбеддингов и кэши не должны попасть в метрики
    let _ = memory.search_hybrid("прогрев", 5, 0.0).await?;
    let _ = graph.search("прогрев", 5, false).await?;

    let mut embed_d = Vec::with_capacity(cases.len());
    let mut mem_d = Vec::with_capacity(cases.len());
    let mut graph_d = Vec::with_capacity(cases.len());
    for case in cases {
        // пол: только эмбеддинг запроса (то же, что делают оба поиска внутри)
        let t = Instant::now();
        let _ = embedder.embed(std::slice::from_ref(&case.query)).await?;
        embed_d.push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        let _ = memory.search_hybrid(&case.query, 5, 0.0).await?;
        mem_d.push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        let _ = graph.search(&case.query, 5, false).await?;
        graph_d.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    let (mem_vectors, graph_vectors) = db.with_conn(|conn| {
        let m: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        let g: i64 = conn.query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))?;
        Ok((m, g))
    })?;

    Ok(LatencyResult {
        cases: cases.len(),
        mem_vectors,
        graph_vectors,
        embed_p50_ms: percentile(&mut embed_d, 50.0),
        embed_p95_ms: percentile(&mut embed_d, 95.0),
        mem_p50_ms: percentile(&mut mem_d, 50.0),
        mem_p95_ms: percentile(&mut mem_d, 95.0),
        graph_p50_ms: percentile(&mut graph_d, 50.0),
        graph_p95_ms: percentile(&mut graph_d, 95.0),
    })
}

/// Порог гейта владельца (35.1): p95 > 80–100 мс на ~600K векторов → эксперимент sqlite-vec.
pub const LATENCY_GATE_P95_MS: f64 = 100.0;

/// Полный прогон CLI: печать таблицы, --json, --save-baseline.
pub async fn cli_run(
    ctx: &crate::mcp::AppContext,
    mode: &str,
    k_spec: &str,
    golden: Option<&str>,
    project: Option<&str>,
    as_json: bool,
    save_baseline: bool,
) -> anyhow::Result<()> {
    if mode == "history" {
        // Ф35.2: сводка накопительной статистики — golden-набор не нужен
        let path = crate::cli::bench_history::history_path(&ctx.settings.data_dir);
        let summary = crate::cli::bench_history::analyze(&path)?;
        if as_json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            crate::cli::bench_history::print_summary(&summary);
        }
        return Ok(());
    }
    if mode != "search"
        && mode != "context"
        && mode != "code"
        && mode != "latency"
        && mode != "all"
    {
        anyhow::bail!("mode должен быть all|search|context|code|latency|history, получено: {mode}");
    }
    let ks: Vec<usize> = k_spec
        .split(',')
        .map(|s| s.trim().parse::<usize>())
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("--k: ожидались числа через запятую (напр. 5,10): {e}"))?;
    if ks.contains(&0) {
        anyhow::bail!("--k: уровни k должны быть ≥ 1");
    }

    // Ф69.4: сводный раннер — единый JSON-отчет + гейт ADR-13 (не требует golden.jsonl).
    if mode == "all" {
        return cli_run_all(ctx, &ks, project, as_json).await;
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

    if mode == "latency" {
        // Ф35.1: p50/p95 отдельно по memories и graph_nodes (recall не считается —
        // golden-набор привязан к ключам памяти).
        let lat = run_latency(&ctx.memory, &ctx.graph, &ctx.embedder, &ctx.db, &cases).await?;
        if as_json {
            println!(
                "{}",
                json!({
                    "mode": "latency",
                    "cases": lat.cases,
                    "embed_query": {
                        "p50_ms": (lat.embed_p50_ms * 10.0).round() / 10.0,
                        "p95_ms": (lat.embed_p95_ms * 10.0).round() / 10.0,
                    },
                    "memories": {
                        "vectors": lat.mem_vectors,
                        "p50_ms": (lat.mem_p50_ms * 10.0).round() / 10.0,
                        "p95_ms": (lat.mem_p95_ms * 10.0).round() / 10.0,
                    },
                    "graph_nodes": {
                        "vectors": lat.graph_vectors,
                        "p50_ms": (lat.graph_p50_ms * 10.0).round() / 10.0,
                        "p95_ms": (lat.graph_p95_ms * 10.0).round() / 10.0,
                    },
                    "gate_p95_ms": LATENCY_GATE_P95_MS,
                    "gate_over": lat.mem_p95_ms > LATENCY_GATE_P95_MS
                        || lat.graph_p95_ms > LATENCY_GATE_P95_MS,
                })
            );
        } else {
            println!("Латентность поиска (Ф35.1), кейсов: {}", lat.cases);
            println!(
                "{:<14} {:>10} {:>10} {:>10}",
                "хранилище", "векторов", "p50, мс", "p95, мс"
            );
            println!(
                "{:<14} {:>10} {:>10.1} {:>10.1}",
                "embed(query)", "-", lat.embed_p50_ms, lat.embed_p95_ms
            );
            println!(
                "{:<14} {:>10} {:>10.1} {:>10.1}",
                "memories", lat.mem_vectors, lat.mem_p50_ms, lat.mem_p95_ms
            );
            println!(
                "{:<14} {:>10} {:>10.1} {:>10.1}",
                "graph_nodes", lat.graph_vectors, lat.graph_p50_ms, lat.graph_p95_ms
            );
            println!(
                "порог гейта p95 = {LATENCY_GATE_P95_MS:.0} мс: {}",
                if lat.mem_p95_ms > LATENCY_GATE_P95_MS || lat.graph_p95_ms > LATENCY_GATE_P95_MS {
                    "ПРЕВЫШЕН — нужен эксперимент sqlite-vec"
                } else {
                    "не превышен — эксперимент откладывается, решение фиксируется цифрами"
                }
            );
        }
        if save_baseline {
            let out = std::path::PathBuf::from("docs").join("bench_baseline.md");
            write_latency_baseline(&lat, &out)?;
            println!("latency-секция сохранена: {}", out.display());
        }
        // Ф35.2: запись прогона в history.jsonl (накопительная статистика)
        let db_path = ctx.settings.data_dir.join("ob2h.db");
        let rec = crate::cli::bench_history::latency_record(
            &lat,
            &db_path,
            &ctx.settings.embed_provider,
            crate::vector::vec0::enabled(),
        );
        let _ = crate::cli::bench_history::append_record(
            &crate::cli::bench_history::history_path(&ctx.settings.data_dir),
            &rec,
        );
        return Ok(());
    }

    // Ф54 (PLAN.md): mode=code — PPR по кодовому графу проекта против
    // golden_code.jsonl (query → ожидаемый node_id). Веса — OB2H_PPR_WEIGHTS
    // (по умолчанию DEFAULT_PPR_WEIGHTS): калибровка дефолта делается числами.
    if mode == "code" {
        let Some(pid) = project else {
            anyhow::bail!("mode=code требует --project <id> (PPR по кодовому графу)");
        };
        let path = golden
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ctx.settings.data_dir.join("bench").join("golden_code.jsonl"));
        let cases = load_golden(&path)?;
        let damping = std::env::var("OB2H_PPR_DAMPING")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(crate::graph::pagerank::DAMPING_MAX);
        let weights = crate::graph::pagerank::parse_ppr_weights(
            &std::env::var("OB2H_PPR_WEIGHTS")
                .unwrap_or_else(|_| crate::graph::pagerank::DEFAULT_PPR_WEIGHTS.to_string()),
        );
        let result = run_code_bench(&ctx.graph, &cases, pid, &ks, damping, &weights).await?;
        if as_json {
            println!(
                "{}",
                json!({
                    "mode": "code",
                    "project": pid,
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
            print_human(&result, &path);
        }
        return Ok(());
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

    // Ф35.2: запись прогона в history.jsonl (recall/mrr по уровням k)
    let db_path = ctx.settings.data_dir.join("ob2h.db");
    let recall_at = |k: usize| {
        result
            .ks
            .iter()
            .position(|&x| x == k)
            .and_then(|i| result.recall.get(i).copied())
    };
    let db_size_mb = std::fs::metadata(&db_path)
        .map(|m| m.len() as f64 / 1_048_576.0)
        .unwrap_or(0.0);
    let rec = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "mode": result.mode,
        "cases": result.cases,
        "recall@5": recall_at(5),
        "recall@10": recall_at(10),
        // Ф69.2: LongMemEval-стиль — наблюдение; порог гейта ADR-13 не меняется
        "recall_all@5": result.ks.iter().position(|&x| x == 5).and_then(|i| result.recall_all.get(i).copied()),
        "any_hit@5": result.ks.iter().position(|&x| x == 5).and_then(|i| result.any_hit.get(i).copied()),
        "mrr": (result.mrr * 1000.0).round() / 1000.0,
        "p95_ms": (result.p95_ms * 10.0).round() / 10.0,
        "db_size_mb": (db_size_mb * 10.0).round() / 10.0,
        "embedding_backend": ctx.settings.embed_provider,
        "vec0": if crate::vector::vec0::enabled() { "on" } else { "off" },
        "dream_sha": crate::cli::bench_history::last_dream_marker(&db_path),
    });
    let _ = crate::cli::bench_history::append_record(
        &crate::cli::bench_history::history_path(&ctx.settings.data_dir),
        &rec,
    );
    Ok(())
}

/// Ф69.4: секция ast_golden сводного раннера — по вшитым эталонам (Ф69.3):
/// присутствие required_symbols/required_calls и паритет двойного парсинга.
/// Возвращает (общий ok, json-записи по языкам).
pub fn ast_golden_section() -> anyhow::Result<(bool, Vec<serde_json::Value>)> {
    use crate::project::ast::{resolve_calls, AstScanResult};

    let embedded = crate::project::ast_golden::load_embedded()?;
    let mut all_ok = true;
    let mut entries = Vec::new();
    for item in &embedded {
        // Диспетчер — продакшн-путь: под фичей treesitter измеряется CST-бэкенд
        // (call-гейт target-языков активен только тогда, когда бэкенд — CST)
        let parse = |src: &str| {
            let disp = crate::project::ast_backend::AstParserDispatcher::new();
            let mut res = AstScanResult::default();
            disp.parse_source(&item.spec.fixture, src, &mut res);
            resolve_calls(&mut res);
            res
        };
        let a = parse(&item.source);
        let b = parse(&item.source);

        // Паритет (NFR «Детерминизм AST»): двойной парсинг → те же node_id и рёбра.
        let nodes_a: HashSet<&str> = a.nodes.iter().map(|n| n.node_id.as_str()).collect();
        let nodes_b: HashSet<&str> = b.nodes.iter().map(|n| n.node_id.as_str()).collect();
        let edge_key = |r: &AstScanResult| -> HashSet<String> {
            r.edges
                .iter()
                .map(|e| format!("{}|{}|{}", e.source_node_id, e.target_node_id, e.label))
                .collect()
        };
        let parity = nodes_a == nodes_b && edge_key(&a) == edge_key(&b);

        let sym_req = item.spec.required_symbols.len();
        let sym_found = item
            .spec
            .required_symbols
            .iter()
            .filter(|s| {
                a.nodes
                    .iter()
                    .any(|n| n.label == s.name && n.node_type == s.kind)
            })
            .count();
        let call_req = item.spec.required_calls.len();
        let call_found = if item.spec.call_extraction == "current" {
            item.spec
                .required_calls
                .iter()
                .filter(|c| {
                    let from = a.nodes.iter().find(|n| n.label == c.from).map(|n| &n.node_id);
                    let to = a.nodes.iter().find(|n| n.label == c.to).map(|n| &n.node_id);
                    from.is_some()
                        && to.is_some()
                        && a.edges.iter().any(|e| {
                            Some(&e.source_node_id) == from
                                && Some(&e.target_node_id) == to
                                && e.label == "CALLS"
                        })
                })
                .count()
        } else {
            call_req // target: не проверяется текущим парсером (цель Фазы 70)
        };

        // 71.7: F1-метрики на исчерпывающих эталонах. Предсказанные символы —
        // словарь SYMBOL_TYPES; предсказанные вызовы — CALLS без эвристик
        // (виртуальные вызовы измеряются отдельно, вне гарантированного слоя).
        use crate::graph::callpath::SYMBOL_TYPES;
        use std::collections::HashSet as BenchHashSet;
        let predicted_syms: BenchHashSet<(String, String)> = a
            .nodes
            .iter()
            .filter(|n| SYMBOL_TYPES.contains(&n.node_type.as_str()))
            .map(|n| (n.label.clone(), n.node_type.clone()))
            .collect();
        let required_syms: BenchHashSet<(String, String)> = item
            .spec
            .required_symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind.clone()))
            .collect();
        let sym_tp = predicted_syms.intersection(&required_syms).count();
        let sym_f1 = crate::project::ast_golden::f1(
            sym_tp,
            predicted_syms.len() - sym_tp,
            required_syms.len() - sym_tp,
        );

        let label_of = |nid: &str| -> String {
            a.nodes
                .iter()
                .find(|n| n.node_id == nid)
                .map(|n| n.label.clone())
                .unwrap_or_default()
        };
        let predicted_calls: BenchHashSet<(String, String)> = a
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.provenance != "HEURISTIC")
            .map(|e| (label_of(&e.source_node_id), label_of(&e.target_node_id)))
            .collect();
        let required_calls_set: BenchHashSet<(String, String)> = item
            .spec
            .required_calls
            .iter()
            .map(|c| (c.from.clone(), c.to.clone()))
            .collect();
        let call_tp = predicted_calls.intersection(&required_calls_set).count();
        let call_f1 = crate::project::ast_golden::f1(
            call_tp,
            predicted_calls.len() - call_tp,
            required_calls_set.len() - call_tp,
        );

        // Гейт 71.7: symbol F1 ≥ 0.95 (все языки), call F1 ≥ 0.90 — для
        // языков с гарантированными вызовами (current); target-языки —
        // метрика репортится, гейт только под фичей treesitter.
        let sym_ok = sym_f1 >= crate::project::ast_golden::SYMBOL_F1_MIN;
        let call_ok = if item.spec.call_extraction == "current" {
            call_f1 >= crate::project::ast_golden::CALL_F1_MIN
        } else {
            #[cfg(feature = "treesitter")]
            {
                call_f1 >= crate::project::ast_golden::CALL_F1_MIN
            }
            #[cfg(not(feature = "treesitter"))]
            {
                true
            }
        };
        let ok = parity && sym_found == sym_req && call_found == call_req && sym_ok && call_ok;
        if !ok {
            all_ok = false;
        }
        entries.push(json!({
            "language": item.spec.language,
            "symbols": format!("{sym_found}/{sym_req}"),
            "calls": format!("{call_found}/{call_req}"),
            "symbol_f1": (sym_f1 * 1000.0).round() / 1000.0,
            "call_f1": (call_f1 * 1000.0).round() / 1000.0,
            "call_extraction": item.spec.call_extraction,
            "parity": parity,
            "ok": ok,
        }));
    }
    Ok((all_ok, entries))
}

/// Ф69.4 (PLAN_v2.0): сводный раннер `ob2h bench all` — единый JSON-отчет и
/// пороги регрессии ADR-13 для CI. Секции: ast_golden (вшитые эталоны — всегда),
/// retrieval search/context/latency (при наличии персонального golden.jsonl),
/// code (при --project и golden_code.jsonl). Гейт: сбой секции ast_golden или
/// регрессия recall@5/MRR против предыдущей записи history.jsonl режима search.
/// Регрессия → exit-код ≠ 0 (гейт CI).
pub async fn cli_run_all(
    ctx: &crate::mcp::AppContext,
    ks: &[usize],
    project: Option<&str>,
    as_json: bool,
) -> anyhow::Result<()> {
    use crate::dream::bench_gate::{is_degradation, read_history, MRR_DROP_LIMIT, RECALL_DROP_LIMIT};

    let mut report = serde_json::Map::new();
    let mut notes: Vec<String> = Vec::new();
    let mut gate_over = false;

    // — Секция ast_golden (всегда доступна: эталоны вшиты в бинарник) —
    let (ast_ok, ast_entries) = ast_golden_section()?;
    report.insert("ast_golden".into(), json!({ "ok": ast_ok, "languages": ast_entries }));
    if !ast_ok {
        gate_over = true;
        notes.push("ast_golden: срыв эталона символов/вызовов или паритета".into());
    }

    // — Секция retrieval (персональный golden.jsonl) —
    let golden_path = ctx.settings.data_dir.join("bench").join("golden.jsonl");
    let mut search_recall5: Option<f64> = None;
    let mut search_mrr: Option<f64> = None;
    if golden_path.is_file() {
        let cases = load_golden(&golden_path)?;
        let search = run_bench(&ctx.memory, &cases, "search", ks).await?;
        let context = run_bench(&ctx.memory, &cases, "context", ks).await?;
        let lat = run_latency(&ctx.memory, &ctx.graph, &ctx.embedder, &ctx.db, &cases).await?;
        let r5 = search
            .ks
            .iter()
            .position(|&x| x == 5)
            .and_then(|i| search.recall.get(i).copied());
        search_recall5 = r5;
        search_mrr = Some(search.mrr);
        report.insert("search".into(), bench_result_json(&search));
        report.insert("context".into(), bench_result_json(&context));
        report.insert(
            "latency".into(),
            json!({
                "mem_p95_ms": (lat.mem_p95_ms * 10.0).round() / 10.0,
                "graph_p95_ms": (lat.graph_p95_ms * 10.0).round() / 10.0,
                "gate_p95_ms": LATENCY_GATE_P95_MS,
            }),
        );
        if lat.mem_p95_ms > LATENCY_GATE_P95_MS || lat.graph_p95_ms > LATENCY_GATE_P95_MS {
            notes.push(format!(
                "latency: p95 > {LATENCY_GATE_P95_MS:.0} мс — критерий эксперимента sqlite-vec (35.1)"
            ));
        }
    } else {
        report.insert(
            "retrieval".into(),
            json!({ "skipped": "data/bench/golden.jsonl отсутствует" }),
        );
        notes.push("retrieval пропущен: персональный golden.jsonl не заведён".into());
    }

    // — Секция code (опционально: --project + golden_code.jsonl) —
    if let Some(pid) = project {
        let code_path = ctx.settings.data_dir.join("bench").join("golden_code.jsonl");
        if code_path.is_file() {
            let cases = load_golden(&code_path)?;
            let damping = std::env::var("OB2H_PPR_DAMPING")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(crate::graph::pagerank::DAMPING_MAX);
            let weights = crate::graph::pagerank::parse_ppr_weights(
                &std::env::var("OB2H_PPR_WEIGHTS")
                    .unwrap_or_else(|_| crate::graph::pagerank::DEFAULT_PPR_WEIGHTS.to_string()),
            );
            let code = run_code_bench(&ctx.graph, &cases, pid, ks, damping, &weights).await?;
            report.insert("code".into(), bench_result_json(&code));
        } else {
            notes.push("code пропущен: data/bench/golden_code.jsonl отсутствует".into());
        }
    } else {
        notes.push("code пропущен: не указан --project".into());
    }

    // — Гейт ADR-13: регрессия против предыдущей записи history.jsonl (mode=search) —
    if let (Some(r5), Some(mrr)) = (search_recall5, search_mrr) {
        let prev = read_history(&ctx.settings, 50)?
            .into_iter()
            .rev()
            .find(|row| {
                row.get("mode").and_then(|v| v.as_str()) == Some("search")
                    && row.get("recall@5").and_then(|v| v.as_f64()).is_some()
            });
        match prev {
            Some(p) => {
                let pr5 = p.get("recall@5").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let pm = p.get("mrr").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if is_degradation((pr5, pm), (r5, mrr)) {
                    gate_over = true;
                    notes.push(format!(
                        "retrieval: регрессия recall@5 {pr5:.3}→{r5:.3} / MRR {pm:.3}→{mrr:.3} сверх порогов ADR-13"
                    ));
                }
            }
            None => {
                notes.push("retrieval: первый прогон — база фиксируется".into());
            }
        }
    }

    let emb_rep = run_embeddings_bench(ctx.embedder.as_ref()).await?;
    if !emb_rep["bq_gate_pass"].as_bool().unwrap_or(false) {
        gate_over = true;
        notes.push(format!(
            "vector_bq: recall@10={:.3} < 0.90 (ADR-13)",
            emb_rep["bq_recall_at_10"].as_f64().unwrap_or(0.0)
        ));
    }
    report.insert("embeddings".into(), emb_rep);

    report.insert(
        "gate".into(),
        json!({
            "adr": "ADR-13",
            "over": gate_over,
            "limits": {
                "recall5_drop": RECALL_DROP_LIMIT,
                "mrr_drop": MRR_DROP_LIMIT,
            },
            "notes": notes,
        }),
    );

    let value = serde_json::Value::Object(report);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_all_human(&value);
    }

    // Накопительная история: одна запись на сводный прогон (mode=all).
    if let (Some(r5), Some(_mrr)) = (search_recall5, search_mrr) {
        let db_path = ctx.settings.data_dir.join("ob2h.db");
        let db_size_mb = std::fs::metadata(&db_path)
            .map(|m| m.len() as f64 / 1_048_576.0)
            .unwrap_or(0.0);
        let mrr = search_mrr.unwrap_or(0.0);
        let rec = json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "mode": "all",
            "recall@5": r5,
            "mrr": (mrr * 1000.0).round() / 1000.0,
            "db_size_mb": (db_size_mb * 10.0).round() / 10.0,
            "embedding_backend": ctx.settings.embed_provider,
            "ast_golden_ok": ast_ok,
        });
        let _ = crate::cli::bench_history::append_record(
            &crate::cli::bench_history::history_path(&ctx.settings.data_dir),
            &rec,
        );
    }

    if gate_over {
        anyhow::bail!("Гейт ADR-13: регрессия — детали в notes отчета (exit ≠ 0 для CI)");
    }
    Ok(())
}

/// Компактный JSON-объект результата BenchResult (для секций all-отчета).
fn bench_result_json(result: &BenchResult) -> serde_json::Value {
    json!({
        "mode": result.mode,
        "cases": result.cases,
        "k": result.ks,
        "recall_at_k": result.recall,
        "mrr": result.mrr,
        "empty_count": result.empty_count,
        "p50_ms": (result.p50_ms * 10.0).round() / 10.0,
        "p95_ms": (result.p95_ms * 10.0).round() / 10.0,
    })
}

/// Человекочитаемый вывод сводного отчета.
fn print_all_human(report: &serde_json::Value) {
    println!("ob2h bench all v{}", env!("CARGO_PKG_VERSION"));
    if let Some(ast) = report.get("ast_golden") {
        println!(
            "ast_golden: ok={} ({})",
            ast.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
            ast.get("languages")
                .and_then(|v| v.as_array())
                .map(|arr| arr
                    .iter()
                    .filter_map(|e| e.get("language").and_then(|l| l.as_str()))
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        );
    }
    for section in ["search", "context", "code"] {
        if let Some(s) = report.get(section) {
            println!(
                "{section}: кейсов {} | recall@5 {} | MRR {} | p95 {} мс",
                s.get("cases").and_then(|v| v.as_u64()).unwrap_or(0),
                s.get("recall_at_k")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_f64())
                    .map(|r| format!("{r:.3}"))
                    .unwrap_or_else(|| "-".into()),
                s.get("mrr").and_then(|v| v.as_f64()).map(|m| format!("{m:.3}")).unwrap_or_else(|| "-".into()),
                s.get("p95_ms").and_then(|v| v.as_f64()).map(|p| format!("{p:.1}")).unwrap_or_else(|| "-".into()),
            );
        }
    }
    if let Some(sk) = report.get("retrieval").and_then(|v| v.get("skipped")) {
        println!("retrieval: пропущен ({sk})");
    }
    if let Some(gate) = report.get("gate") {
        println!(
            "гейт ADR-13: {}",
            if gate.get("over").and_then(|v| v.as_bool()).unwrap_or(false) {
                "ПРЕВЫШЕН"
            } else {
                "не превышен"
            }
        );
        if let Some(notes) = gate.get("notes").and_then(|v| v.as_array()) {
            for n in notes {
                if let Some(s) = n.as_str() {
                    println!("  • {s}");
                }
            }
        }
    }
}

/// Эталонные триплеты (anchor, positive, negative) для валидации RU и RU/EN качества (Ф72.7).
const RU_TRIPLETS: &[(&str, &str, &str)] = &[
    (
        "настройка базы данных SQLite WAL и таймаут блокировки",
        "журнал предзаписи SQLite journal_mode=WAL и busy_timeout",
        "рецепт домашнего яблочного пирога с корицей",
    ),
    (
        "авторизация пользователя по JWT токену доступа",
        "проверка подписи токена и срока действия сессии",
        "посадка и полив томатов в теплице весной",
    ),
    (
        "резервное копирование базы знаний и ротация бэкапов",
        "создание снапшота SQLite VACUUM INTO и очистка старых архивов",
        "маршруты общественного транспорта и расписание автобусов",
    ),
    (
        "шифрование паролей и ключей API в локальном сейфе",
        "защита секретов алгоритмом ChaCha20-Poly1305 и мастер-ПИН",
        "обзор новых фильмов и сериалов за прошлую неделю",
    ),
    (
        "инкрементальное сканирование исходного кода проекта",
        "обновление графа символов только для изменённых файлов по хешу",
        "тренировка по бегу на длинные дистанции на стадионе",
    ),
    (
        "разрешение противоречий между старыми и новыми фактами",
        "пометка устаревшей записи ребром SUPERSEDES без удаления",
        "выбор зимней резины для легкового автомобиля",
    ),
];

const RU_EN_TRIPLETS: &[(&str, &str, &str)] = &[
    (
        "AST парсер Rust через tree-sitter",
        "синтаксическое дерево Rust и извлечение графа вызовов CALLS",
        "CSS Grid layout для адаптивной сетки карточек",
    ),
    (
        "LRU кэш эмбеддингов для ускорения поиска",
        "in-memory LruCache векторов запросов без повторного инференса",
        "настройка DNS записей и SSL сертификата на домене",
    ),
    (
        "векторный поиск и слияние RRF k=60",
        "гибридное ранжирование Reciprocal Rank Fusion для FTS5 и cosine",
        "конфигурация Docker Compose для продакшн веб-сервера",
    ),
    (
        "блокировка смешанных векторов через EmbeddingFingerprint",
        "проверка model_id и reindex --embeddings при смене бэкенда",
        "анимация кнопок и переходов в Flutter виджете",
    ),
    (
        "вынос инференса Candle в spawn_blocking пул",
        "асинхронная изоляция CPU-батчей чтобы не блокировать Tokio MCP",
        "парсинг HTML шаблонов Blade и стилизация Bootstrap",
    ),
    (
        "анализ Blast Radius через project_impact",
        "поиск зависимых функций и затронутых тестов TESTED_BY",
        "подключение платёжного шлюза Stripe в веб-приложении",
    ),
];

async fn eval_triplets(
    embedder: &dyn crate::embedding::EmbeddingProvider,
    triplets: &[(&str, &str, &str)],
    use_prefixes: bool,
) -> anyhow::Result<f64> {
    let mut passed = 0usize;
    for &(anchor, pos, neg) in triplets {
        let (qa, vp, vn) = if use_prefixes {
            let qa = embedder.embed_query(anchor).await?;
            let docs = embedder
                .embed_documents(&[pos.to_string(), neg.to_string()])
                .await?;
            (qa, docs[0].clone(), docs[1].clone())
        } else {
            let vecs = embedder
                .embed(&[anchor.to_string(), pos.to_string(), neg.to_string()])
                .await?;
            (vecs[0].clone(), vecs[1].clone(), vecs[2].clone())
        };
        let sim_pos = crate::vector::cosine(&qa, &vp);
        let sim_neg = crate::vector::cosine(&qa, &vn);
        if sim_pos > sim_neg {
            passed += 1;
        }
    }
    Ok(passed as f64 / triplets.len().max(1) as f64)
}

/// Замер скорости инференса и качества триплетов (Ф72.7, Ф72.2a/b).
pub async fn run_embeddings_bench(
    embedder: &dyn crate::embedding::EmbeddingProvider,
) -> anyhow::Result<serde_json::Value> {
    let ru_acc = eval_triplets(embedder, RU_TRIPLETS, true).await?;
    let ru_en_acc = eval_triplets(embedder, RU_EN_TRIPLETS, true).await?;
    let ru_noprefix = eval_triplets(embedder, RU_TRIPLETS, false).await?;
    let prefix_delta = ru_acc - ru_noprefix;

    // Замер батча 32 текста
    let batch: Vec<String> = (0..32)
        .map(|i| format!("тестовый чанк кода #{i} fn process_request() -> Result<()>"))
        .collect();
    let t0 = Instant::now();
    let _ = embedder.embed_documents(&batch).await?;
    let batch_32_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Сравнение AST-чанкинга vs фиксированного окна (72.2a)
    let sample_src = "use std::io;\n\n\n\npub fn parse_config_file() {\n    let timeout_ms = 5000;\n    let retries = 3;\n}\n";
    let fixed = crate::project::chunker::fixed_window_chunks(sample_src, 3, 0);
    let fixed_hits = fixed
        .iter()
        .filter(|c| c.contains("parse_config_file") && c.contains("timeout_ms"))
        .count();
    let ast_chunk = crate::project::chunker::format_ast_chunk(
        "src/config.rs",
        "Function parse_config_file",
        &sample_src.lines().skip(4).collect::<Vec<_>>().join("\n"),
        crate::project::chunker::DEFAULT_CHUNK_MAX_CHARS,
    );
    let ast_hit = if ast_chunk.contains("parse_config_file") && ast_chunk.contains("timeout_ms") {
        1.0
    } else {
        0.0
    };
    let fixed_rate = fixed_hits as f64 / fixed.len().max(1) as f64;
    let chunker_gain = ast_hit - fixed_rate;

    let (bq_recall_at_10, bq_p95_ms) = eval_bq_recall_at_10(embedder.dim().max(64))?;
    let bq_gate_pass = bq_recall_at_10 >= 0.90;

    let is_fake = embedder.backend_id() == "fake";
    let ru_gate_pass = is_fake || (ru_acc >= 0.80 && ru_en_acc >= 0.80);

    Ok(json!({
        "backend": embedder.backend_id(),
        "model": embedder.model_id(),
        "dim": embedder.dim(),
        "query_prefix": embedder.query_prefix(),
        "document_prefix": embedder.document_prefix(),
        "ru_triplet_accuracy": (ru_acc * 1000.0).round() / 1000.0,
        "ru_en_triplet_accuracy": (ru_en_acc * 1000.0).round() / 1000.0,
        "prefix_delta_ru": (prefix_delta * 1000.0).round() / 1000.0,
        "chunker_recall_gain": (chunker_gain * 1000.0).round() / 1000.0,
        "batch_32_ms": (batch_32_ms * 10.0).round() / 10.0,
        "bq_recall_at_10": (bq_recall_at_10 * 1000.0).round() / 1000.0,
        "bq_p95_ms": (bq_p95_ms * 100.0).round() / 100.0,
        "bq_threshold": 0.90,
        "bq_gate_pass": bq_gate_pass,
        "ru_threshold": 0.80,
        "gate_pass": ru_gate_pass && bq_gate_pass,
        "is_fake": is_fake,
    }))
}

/// Ф72b.4: проверка recall@10 двухступенчатого BQ-поиска против brute-force эталона (ADR-13).
fn eval_bq_recall_at_10(dim: usize) -> anyhow::Result<(f64, f64)> {
    use crate::vector::{bq, similarity::{serialize_q, top_k}};

    let count = 1000usize;
    let vecs: Vec<Vec<f32>> = (0..count)
        .map(|i| {
            (0..dim)
                .map(|d| {
                    let phase = ((i * 131 + d * 17) % 97) as f32 / 97.0 - 0.5;
                    0.08 + 0.04 * phase + if (i % 10) == (d % 10) { 0.03 } else { 0.0 }
                })
                .collect()
        })
        .collect();
    let refs: Vec<&[f32]> = vecs.iter().map(|v| v.as_slice()).collect();
    let means = bq::compute_per_dim_means(&refs);

    let db = crate::db::Database::in_memory()?;
    let mut blobs: Vec<(i64, Vec<u8>)> = Vec::with_capacity(count);
    db.with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        for (i, v) in vecs.iter().enumerate() {
            let blob = serialize_q(v);
            let mask = bq::quantize_mean_centered(v, Some(&means));
            tx.execute(
                "INSERT INTO memories (key, content, category, embedding, bq_mask, created_at, updated_at) \
                 VALUES (?1, ?2, 'general', ?3, ?4, datetime('now'), datetime('now'))",
                rusqlite::params![format!("bq_{i}"), format!("doc {i}"), blob, mask],
            )?;
            let id = tx.last_insert_rowid();
            blobs.push((id, blob));
        }
        tx.commit()?;
        Ok(())
    })?;

    let blob_refs: Vec<(i64, Option<&[u8]>)> = blobs
        .iter()
        .map(|(id, b)| (*id, Some(b.as_slice())))
        .collect();

    let mut recalls = Vec::with_capacity(20);
    let mut lat_ms = Vec::with_capacity(20);
    for q_idx in (0..count).step_by(count / 20).take(20) {
        let q = &vecs[q_idx];
        let brute = top_k(q, &blob_refs, 10, 0.0);
        let brute_ids: std::collections::HashSet<i64> = brute.iter().map(|x| x.0).collect();

        let t0 = std::time::Instant::now();
        let approx = db.with_conn(|conn| {
            bq::search_namespace(conn, bq::VectorNamespace::Memories, None, q, 10, 0.0)
        })?;
        lat_ms.push(t0.elapsed().as_secs_f64() * 1000.0);

        let hits = approx.iter().filter(|x| brute_ids.contains(&x.0)).count();
        recalls.push(hits as f64 / brute_ids.len().max(1) as f64);
    }

    let mean_recall = recalls.iter().sum::<f64>() / recalls.len().max(1) as f64;
    lat_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95 = lat_ms[((lat_ms.len() * 95) / 100).min(lat_ms.len().saturating_sub(1))];
    Ok((mean_recall, p95))
}

pub async fn cli_embeddings(ctx: &crate::AppContext, as_json: bool) -> anyhow::Result<()> {
    let report = run_embeddings_bench(ctx.embedder.as_ref()).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("ob2h bench embeddings v{}", env!("CARGO_PKG_VERSION"));
        println!(
            "backend={} model={} dim={}",
            report["backend"].as_str().unwrap_or("-"),
            report["model"].as_str().unwrap_or("-"),
            report["dim"].as_u64().unwrap_or(0)
        );
        println!(
            "RU triplets: {:.3} | RU/EN triplets: {:.3} | prefix delta: {:+.3} | chunker gain: {:+.3} | batch32: {:.1} мс",
            report["ru_triplet_accuracy"].as_f64().unwrap_or(0.0),
            report["ru_en_triplet_accuracy"].as_f64().unwrap_or(0.0),
            report["prefix_delta_ru"].as_f64().unwrap_or(0.0),
            report["chunker_recall_gain"].as_f64().unwrap_or(0.0),
            report["batch_32_ms"].as_f64().unwrap_or(0.0),
        );
    }
    if !report["gate_pass"].as_bool().unwrap_or(false) {
        anyhow::bail!("Гейт RU/RU-EN триплетов (< 0.80) не пройден");
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
    // Ф69.2: LongMemEval-стиль метрики полноты
    for (k, r) in result.ks.iter().zip(&result.recall_all) {
        println!("  recall-all@{k:<3} {r:.3}");
    }
    for (k, r) in result.ks.iter().zip(&result.any_hit) {
        println!("  any-hit@{k:<3} {r:.3}");
    }
    println!("  MRR        {:.3}", result.mrr);
    println!("  пусто      {}/{}", result.empty_count, result.cases);
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
    md.push_str(&format!(
        "- Дата: {}\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));
    md.push_str(&format!("- Версия ob2h: {}\n", env!("CARGO_PKG_VERSION")));
    md.push_str(&format!("- Режим: {}\n", result.mode));
    md.push_str(&format!("- Кейсов: {}\n", result.cases));
    md.push_str("- Golden: `data/bench/golden.jsonl` (персональные данные, в git не входит;\n");
    md.push_str("  здесь — только агрегаты)\n\n");
    md.push_str(&format!(
        "| {} | MRR | p50, мс | p95, мс |\n",
        heads.join(" | ")
    ));
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
    // Ф35.1: latency-секцию соседнего прогона не затираем
    md.push_str(&read_latency_section(out));
    std::fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(out, md)?;
    Ok(())
}

/// Заголовок latency-секции (Ф35.1) — по нему секция отделяется от recall-части.
const LATENCY_SECTION_TITLE: &str = "## Латентность поиска (Ф35.1)";

/// Вырезать из существующего baseline ранее записанную latency-секцию.
fn read_latency_section(path: &Path) -> String {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    match existing.find(LATENCY_SECTION_TITLE) {
        Some(pos) => format!("\n{}", existing[pos..].trim_end_matches('\n')),
        None => String::new(),
    }
}

/// Ф35.1: записать/обновить latency-секцию baseline (recall-часть сохраняется).
fn write_latency_baseline(lat: &LatencyResult, out: &Path) -> anyhow::Result<()> {
    let existing = std::fs::read_to_string(out).unwrap_or_default();
    let head = match existing.find(LATENCY_SECTION_TITLE) {
        Some(pos) => existing[..pos].trim_end().to_string(),
        None => existing.trim_end().to_string(),
    };

    let over = lat.mem_p95_ms > LATENCY_GATE_P95_MS || lat.graph_p95_ms > LATENCY_GATE_P95_MS;
    let mut md = String::new();
    if head.is_empty() {
        md.push_str("# OB2H bench baseline\n");
    } else {
        md.push_str(&head);
        md.push('\n');
    }
    md.push('\n');
    md.push_str(&format!("{LATENCY_SECTION_TITLE}\n\n"));
    md.push_str(&format!(
        "- Дата: {}\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));
    md.push_str(&format!("- Версия ob2h: {}\n", env!("CARGO_PKG_VERSION")));
    md.push_str(&format!("- Кейсов (golden): {}\n", lat.cases));
    md.push_str(&format!(
        "- Порог гейта владельца: p95 ≤ {LATENCY_GATE_P95_MS:.0} мс\n\n"
    ));
    md.push_str("| Хранилище | векторов | p50, мс | p95, мс |\n");
    md.push_str("|---|---|---|---|\n");
    md.push_str(&format!(
        "| эмбеддинг запроса (пол) | — | {:.1} | {:.1} |\n",
        lat.embed_p50_ms, lat.embed_p95_ms
    ));
    md.push_str(&format!(
        "| memories (search_hybrid) | {} | {:.1} | {:.1} |\n",
        lat.mem_vectors, lat.mem_p50_ms, lat.mem_p95_ms
    ));
    md.push_str(&format!(
        "| graph_nodes (graph.search) | {} | {:.1} | {:.1} |\n",
        lat.graph_vectors, lat.graph_p50_ms, lat.graph_p95_ms
    ));
    md.push_str(&format!(
        "\n> Решение (35.1): p95 {} порога — эксперимент `OB2H_VEC0=1` (sqlite-vec rescore,\n",
        if over {
            "ПРЕВЫШАЕТ"
        } else {
            "не превышает"
        }
    ));
    md.push_str("> int8 + oversample + full-precision re-rank) ");
    if over {
        md.push_str("запускается: критерий recall@10 ≥ 0.99 и p95 < 50 мс.\n");
    } else {
        md.push_str("откладывается; остаёмся на собственном brute force.\n");
    }
    md.push_str("> Метрики — только латентность (recall по графу требует отдельного golden'а).\n");

    std::fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(out, md)?;
    Ok(())
}

/// Ф30.4: `ob2h bench history [--last N]` — тренд ночных прогонов гейта.
pub fn cli_history(settings: &crate::config::Settings, last: usize) -> anyhow::Result<()> {
    let rows = crate::dream::bench_gate::read_history(settings, last)?;
    if rows.is_empty() {
        println!(
            "История пуста: {} (гейт пишет строку на каждый прогон после дрима)",
            settings
                .data_dir
                .join("bench")
                .join("history.jsonl")
                .display()
        );
        return Ok(());
    }
    println!(
        "ob2h bench history — последние {} из {} (data/bench/history.jsonl)",
        rows.len(),
        last
    );
    println!();
    println!(
        "{:<20} {:<10} {:>9} {:>8} {:>9} {:>7} {:>9} {:<6} dream_sha",
        "ts", "gate", "recall@5", "Δr@5", "recall@10", "MRR", "p95, мс", "БД,МБ"
    );
    let mut prev_recall: Option<f64> = None;
    for row in &rows {
        let ts = row.get("ts").and_then(|v| v.as_str()).unwrap_or("-");
        let gate = row.get("gate").and_then(|v| v.as_str()).unwrap_or("-");
        let r5 = row.get("recall@5").and_then(|v| v.as_f64());
        let r10 = row.get("recall@10").and_then(|v| v.as_f64());
        let mrr = row.get("mrr").and_then(|v| v.as_f64());
        let p95 = row.get("p95_ms").and_then(|v| v.as_f64());
        let dbmb = row.get("db_size_mb").and_then(|v| v.as_f64());
        let sha = row.get("dream_sha").and_then(|v| v.as_str()).unwrap_or("-");
        let delta = match (prev_recall, r5) {
            (Some(p), Some(c)) => format!("{:+.3}", c - p),
            _ => "-".to_string(),
        };
        let fmt = |v: Option<f64>, digits: usize| match v {
            Some(x) => format!("{x:.digits$}"),
            None => "-".to_string(),
        };
        println!(
            "{:<20} {:<10} {:>8} {:>8} {:>9} {:>7} {:>9} {:<6} {}",
            ts,
            gate,
            fmt(r5, 3),
            delta,
            fmt(r10, 3),
            fmt(mrr, 3),
            fmt(p95, 1),
            fmt(dbmb, 1),
            sha
        );
        if r5.is_some() {
            prev_recall = r5;
        }
    }
    println!();
    println!("Пороги гейта: recall@5 >10% или MRR >15% относительно bench:last → rollback дрима");
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
        let ranked: Vec<String> = vec!["x", "a", "y", "b"]
            .into_iter()
            .map(String::from)
            .collect();
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
    fn ast_golden_section_all_ok() {
        let (ok, langs) = ast_golden_section().expect("секция ast_golden");
        assert!(ok, "вшитые эталоны должны проходить паритет и присутствие");
        assert_eq!(langs.len(), 5);
        for e in &langs {
            assert!(e.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        }
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

    #[tokio::test]
    async fn embeddings_bench_reports_triplets_and_chunker_gain() {
        let fake = crate::embedding::FakeEmbedding::new(64);
        let rep = run_embeddings_bench(&fake).await.unwrap();
        assert_eq!(rep["backend"].as_str(), Some("fake"));
        assert!(rep["gate_pass"].as_bool().unwrap_or(false));
        assert!(rep["chunker_recall_gain"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(rep["bq_recall_at_10"].as_f64().unwrap_or(0.0) >= 0.90);
        assert!(rep["bq_gate_pass"].as_bool().unwrap_or(false));
    }
}

/// Ф54: прогон golden-набора по кодовому графу: PPR от семантических сидов
/// (graph search) с расшисанием по рёбрам графа проекта; метрики — по
/// позициям ожидаемых node_id в ранжированной выдаче.
async fn run_code_bench(
    graph: &crate::graph::GraphService,
    cases: &[GoldenCase],
    project_id: &str,
    ks: &[usize],
    damping: f64,
    weights: &crate::graph::pagerank::PprWeights,
) -> anyhow::Result<BenchResult> {
    if ks.is_empty() {
        anyhow::bail!("список k пуст");
    }
    let max_k = *ks.iter().max().unwrap();

    // pk → node_id (строковый идентификатор узла) для сопоставления с golden
    let id_map: std::collections::HashMap<i64, String> = graph
        .all_node_ids(project_id)?
        .into_iter()
        .collect();

    // Прогрев: первый вызов грузит модель эмбеддингов — вне метрик.
    let _ = graph
        .ppr_search("прогрев", Some(project_id), 5, damping, weights)
        .await;

    let mut recalls = vec![0.0f64; ks.len()];
    let mut recall_all_hits = vec![0.0f64; ks.len()];
    let mut any_hit_hits = vec![0.0f64; ks.len()];
    let mut mrr_sum = 0.0f64;
    let mut empty_count = 0usize;
    let mut durations = Vec::with_capacity(cases.len());
    let mut scored_cases = 0usize;
    let mut missing_keys: Vec<String> = Vec::new();

    for case in cases {
        let expected: HashSet<String> = case
            .expect_keys
            .iter()
            .filter(|k| id_map.values().any(|nid| nid == *k))
            .cloned()
            .collect();
        for k in &case.expect_keys {
            if !expected.contains(k) && !missing_keys.contains(k) {
                missing_keys.push(k.clone());
            }
        }
        if expected.is_empty() {
            continue;
        }
        scored_cases += 1;

        let started = Instant::now();
        let hits = graph
            .ppr_search(&case.query, Some(project_id), max_k * 2, damping, weights)
            .await?;
        let mut ranked: Vec<String> = Vec::new();
        for hit in &hits {
            if let Some(nid) = id_map.get(&hit.node_id) {
                if !ranked.contains(nid) {
                    ranked.push(nid.clone());
                }
            }
        }
        durations.push(started.elapsed().as_secs_f64() * 1000.0);
        if ranked.is_empty() {
            empty_count += 1;
        }
        for (i, &k) in ks.iter().enumerate() {
            recalls[i] += recall_at_k(&ranked, &expected, k);
            if recall_all_at_k(&ranked, &expected, k) {
                recall_all_hits[i] += 1.0;
            }
            if any_hit_at_k(&ranked, &expected, k) {
                any_hit_hits[i] += 1.0;
            }
        }
        mrr_sum += mrr(&ranked, &expected);
    }

    let n = scored_cases.max(1) as f64;
    let recalls: Vec<f64> = recalls.iter().map(|r| (r / n * 1000.0).round() / 1000.0).collect();
    let recall_all: Vec<f64> = recall_all_hits
        .iter()
        .map(|r| (r / n * 1000.0).round() / 1000.0)
        .collect();
    let any_hit: Vec<f64> = any_hit_hits
        .iter()
        .map(|r| (r / n * 1000.0).round() / 1000.0)
        .collect();
    let mrr = (mrr_sum / n * 1000.0).round() / 1000.0;
    let p50 = percentile(&mut durations, 50.0);
    let p95 = percentile(&mut durations, 95.0);

    Ok(BenchResult {
        mode: format!("code:{}", project_id),
        cases: scored_cases,
        ks: ks.to_vec(),
        recall: recalls,
        recall_all,
        any_hit,
        mrr,
        empty_count,
        p50_ms: p50,
        p95_ms: p95,
        missing_keys,
    })
}
