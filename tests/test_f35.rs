//! Ф35 (PLAN_v1.4): латентность, выдача, гигиена.
//! - 35.3: MCP tool annotations в `tools/list` (readOnlyHint/destructiveHint)
//! - 35.4: ретеншн workspace-логов — архивация старых daily-логов в archive/YYYY-MM.jsonl.gz
//! - 35.1: latency-режим bench — счётчики и метрики по обоим хранилищам

use std::io::Read as _;
use std::sync::Arc;

use ob2h::embedding::FakeEmbedding;
use ob2h::mcp::tools::{annotations_for, list_tools};
use ob2h::workspace::Workspace;
use serde_json::Value;

/// 35.3: аннотации видны в tools/list; read-only набор и destructive точны.
#[test]
fn annotations_present_and_semantically_correct() {
    let tools = list_tools();
    assert!(!tools.is_empty());

    // read-only инструменты аннотированы readOnlyHint=true и не destructive
    for name in [
        "memory_search",
        "memory_context",
        "graph_search",
        "graph_reason",
        "graph_stats",
        "omnes_stats",
        "project_report",
        "project_impact",
        "workspace_read",
    ] {
        let ann = annotations_for(name).unwrap_or_else(|| panic!("{name}: нет аннотаций"));
        assert_eq!(ann["readOnlyHint"], Value::Bool(true), "{name}");
        assert_eq!(ann["destructiveHint"], Value::Bool(false), "{name}");
    }

    // destructive — только явные разрушители
    for name in ["memory_forget", "dream_restore"] {
        let ann = annotations_for(name).unwrap_or_else(|| panic!("{name}: нет аннотаций"));
        assert_eq!(ann["destructiveHint"], Value::Bool(true), "{name}");
        assert_eq!(ann["readOnlyHint"], Value::Bool(false), "{name}");
    }

    // пишущие, но не разрушительные — без аннотаций (не выдаём readOnlyHint ложно)
    for name in ["memory_save", "memory_merge", "workspace_write", "project_scan", "dream_run"] {
        assert!(
            annotations_for(name).is_none(),
            "{name}: пишущий инструмент не должен иметь аннотаций"
        );
    }

    // каждая аннотированная запись ссылается на существующий инструмент
    let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    for name in ["memory_search", "memory_forget", "dream_restore", "workspace_read"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} (аннотирован) должен присутствовать в tools/list"
        );
    }
}

/// 35.3: сериализация tools/list несёт поле `annotations` (как в JSON-RPC ответе).
#[test]
fn tools_list_serialization_contains_annotations() {
    let tools = list_tools();
    let json: Vec<Value> = tools
        .iter()
        .map(|t| {
            let mut v = serde_json::to_value(t).expect("serialize tool");
            if let Some(a) = annotations_for(&t.name) {
                v["annotations"] = a;
            }
            v
        })
        .collect();
    let read_only = json
        .iter()
        .filter(|t| t["annotations"]["readOnlyHint"] == Value::Bool(true))
        .count();
    let destructive = json
        .iter()
        .filter(|t| t["annotations"]["destructiveHint"] == Value::Bool(true))
        .count();
    assert!(read_only >= 10, "read-only инструментов: {read_only}");
    assert_eq!(destructive, 2, "destructive: {destructive}");
    // контракт аргументов не тронут: inputSchema у всех на месте
    assert!(json.iter().all(|t| t["inputSchema"].is_object()));
}

/// 35.4: старые daily-логи упаковываются в месячный архив, свежие и «чужие» не трогаются.
#[test]
fn archive_old_logs_moves_only_stale_canonical_logs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = Workspace::new(tmp.path());
    let daily = ws.daily_dir();
    std::fs::create_dir_all(&daily).expect("daily");

    // старые: разные месяцы (архив месяц-в-месяц), свежий и чужой формат
    std::fs::write(daily.join("2026-05-01.jsonl"), "{\"a\":1}\n").expect("old1");
    std::fs::write(daily.join("2026-05-02.jsonl"), "{\"a\":2}\n").expect("old2");
    std::fs::write(daily.join("2026-06-10.jsonl"), "{\"b\":1}\n").expect("old3");
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    std::fs::write(daily.join(format!("{today}.jsonl")), "{\"fresh\":1}\n").expect("fresh");
    std::fs::write(daily.join("omnes-agent-2026-05-01.jsonl"), "{\"foreign\":1}\n").expect("foreign");

    let archived = ws.archive_old_logs(90).expect("archive");
    let mut archived = archived;
    archived.sort();
    assert_eq!(
        archived,
        vec![
            "2026-05-01.jsonl".to_string(),
            "2026-05-02.jsonl".to_string(),
            "2026-06-10.jsonl".to_string()
        ],
        "архивированы только канонические старые логи"
    );

    // архив существует и содержит исходные строки (архивация, не удаление)
    let may = ws.root().join("archive").join("2026-05.jsonl.gz");
    let jun = ws.root().join("archive").join("2026-06.jsonl.gz");
    assert!(may.is_file() && jun.is_file());
    assert_eq!(gunzip(&may), "{\"a\":1}\n{\"a\":2}\n", "оба майских дня в одном месячном архиве");
    assert_eq!(gunzip(&jun), "{\"b\":1}\n");

    // свежий и «чужой» файлы остались на месте
    assert!(daily.join(format!("{today}.jsonl")).is_file(), "свежий лог не тронут");
    assert!(daily.join("omnes-agent-2026-05-01.jsonl").is_file(), "чужой писатель не тронут");

    // идемпотентность: повторный прогон ничего не архивирует
    let again = ws.archive_old_logs(90).expect("archive 2");
    assert!(again.is_empty(), "повторный прогон — no-op: {again:?}");
    assert_eq!(gunzip(&may), "{\"a\":1}\n{\"a\":2}\n", "архив не задублирован");
}

/// 35.4: на живом ретеншне (90 дней) свежие логи не уезжают в архив.
#[test]
fn archive_respects_retention_window() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = Workspace::new(tmp.path());
    let d = ws.daily_dir();
    std::fs::create_dir_all(&d).expect("daily");
    // 30 дней назад — внутри окна 90 дней
    let recent = (chrono::Utc::now() - chrono::Duration::days(30))
        .format("%Y-%m-%d")
        .to_string();
    std::fs::write(d.join(format!("{recent}.jsonl")), "{}\n").expect("recent");
    assert!(ws.archive_old_logs(90).expect("archive").is_empty());
    assert!(d.join(format!("{recent}.jsonl")).is_file());
    // окно 7 дней — тот же файл уже старше окна
    let moved = ws.archive_old_logs(7).expect("archive 7");
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!(!d.join(format!("{recent}.jsonl")).exists());
}

/// 35.1: latency-режим меряет оба хранилища на синтетике (числа неотрицательны,
/// счётчики берутся из БД).
#[tokio::test]
async fn latency_mode_measures_both_stores() {
    use ob2h::cli::bench::{load_golden, run_latency};
    use ob2h::db::Database;
    use ob2h::graph::GraphService;
    use ob2h::memory::MemoryService;

    let tmp = tempfile::tempdir().expect("tempdir");
    let golden_path = tmp.path().join("golden.jsonl");
    let db = Database::in_memory().expect("db");
    let embedder: Arc<FakeEmbedding> = Arc::new(FakeEmbedding::new(384));
    let memory = MemoryService::new(db.clone(), embedder.clone());
    memory
        .save("латентность памяти синтетика", Some("lat-1"), "facts", 0.5, "chat", None)
        .await
        .expect("save");
    let graph = GraphService::new(db.clone(), embedder.clone());
    let typed: Arc<dyn ob2h::embedding::EmbeddingProvider> = embedder;

    std::fs::write(
        &golden_path,
        "{\"query\":\"латентность\",\"expect_keys\":[\"lat-1\"]}\n",
    )
    .expect("golden");
    let cases = load_golden(&golden_path).expect("load golden");

    let lat = run_latency(&memory, &graph, &typed, &db, &cases)
        .await
        .expect("latency");
    assert_eq!(lat.cases, 1);
    assert_eq!(lat.mem_vectors, 1, "счётчик memories из БД");
    assert_eq!(lat.graph_vectors, 0, "граф пуст");
    assert!(lat.embed_p50_ms >= 0.0 && lat.embed_p95_ms >= 0.0, "пол измеряется отдельно");
    assert!(lat.mem_p50_ms >= 0.0 && lat.mem_p95_ms >= 0.0);
    assert!(lat.graph_p50_ms >= 0.0 && lat.graph_p95_ms >= 0.0);
}

fn gunzip(path: &std::path::Path) -> String {
    let raw = std::fs::read(path).expect("archive read");
    let mut s = String::new();
    flate2::read::GzDecoder::new(&raw[..])
        .read_to_string(&mut s)
        .expect("gunzip");
    s
}
