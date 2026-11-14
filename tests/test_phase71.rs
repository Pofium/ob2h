//! Фаза 71 (PLAN_v2.0): межфайловое разрешение символов (71.4),
//! co-change граф (71.8), TESTED_BY/confidence в project_impact (71.6),
//! F1-гейты golden (71.7) и SFC-инъекции (71.5).

use std::collections::HashSet;
use std::path::PathBuf;

use ob2h::project::ast::{resolve_calls, AstCodeExtractor, AstScanResult};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ast_golden")
}

fn parse_regex(rel_path: &str, source: &str) -> AstScanResult {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    ex.parse_file(rel_path, source, &mut res);
    resolve_calls(&mut res);
    res
}

// ——— 71.4: file-scope import table — межфайловое разрешение символов ———

/// Rust: `use crate::helpers;` + `helpers::run()` — CALLS-ребро в файл модуля
/// (RESOLVED, контекст typepass:qualified).
#[test]
fn import_table_qualified_call_rust() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/helpers.rs"),
        "pub fn run() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "use crate::helpers;\n\npub fn main_app() {\n    helpers::run();\n}\n",
    )
    .unwrap();

    let ex = AstCodeExtractor::new();
    // диспетчер — продакшн-путь: под treesitter даёт CST-квалификаторы
    let mut res = ex.scan_directory_with(root, None, &mut |rel, content, out| {
        ob2h::project::ast_backend::AstParserDispatcher::new().parse_source(rel, content, out)
    });
    let resolved = resolve_calls(&mut res);
    assert!(resolved >= 1, "хотя бы одно ребро вызова; рёбра: {:?}", res
        .edges
        .iter()
        .map(|e| format!("{}:{}:{}", e.label, e.source_node_id, e.target_node_id))
        .collect::<Vec<_>>());

    let edge = res.edges.iter().find(|e| {
        e.label == "CALLS"
            && e.source_node_id == "fn:src/main.rs:main_app"
            && e.target_node_id == "fn:src/helpers.rs:run"
    });
    assert!(
        edge.is_some(),
        "ребро main_app → helpers::run не найдено; рёбра: {:?}",
        res.edges
            .iter()
            .map(|e| format!("{}:{}:{}", e.label, e.source_node_id, e.target_node_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(edge.unwrap().provenance, "RESOLVED");
    // regex-бэкенд резолвит через import_paths (typepass:import), CST —
    // через квалификатор (typepass:qualified); оба точные (не эвристика)
    #[cfg(feature = "treesitter")]
    assert!(edge.unwrap().context.starts_with("typepass:qualified:"));
}

/// Python: `import helpers` + `helpers.load()` — межфайловое CALLS-ребро.
#[test]
fn import_table_qualified_call_python() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("helpers.py"), "def load():\n    return 1\n").unwrap();
    std::fs::write(
        root.join("app.py"),
        "import helpers\n\ndef main_app():\n    helpers.load()\n",
    )
    .unwrap();

    let ex = AstCodeExtractor::new();
    let mut res = ex.scan_directory_with(root, None, &mut |rel, content, out| {
        ex.parse_file(rel, content, out);
        "regex".to_string()
    });
    resolve_calls(&mut res);

    let edge = res.edges.iter().find(|e| {
        e.label == "CALLS"
            && e.source_node_id == "fn:app.py:main_app"
            && e.target_node_id == "fn:helpers.py:load"
    });
    assert!(
        edge.is_some(),
        "ребро main_app → helpers.load не найдено; рёбра: {:?}",
        res.edges
            .iter()
            .map(|e| format!("{}:{}:{}", e.label, e.source_node_id, e.target_node_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(edge.unwrap().provenance, "RESOLVED");
}

/// TS: `import * as utils from './utils'` + `utils.helper()` — namespace-вызов
/// резолвится по таблице импортов (CST-бэкенд: ImportRef namespace_import).
#[cfg(feature = "treesitter")]
#[test]
fn import_table_namespace_call_ts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("utils.ts"),
        "export function helper(): number {\n  return 1;\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("app.ts"),
        "import * as utils from './utils';\n\nexport function mainApp(): void {\n  utils.helper();\n}\n",
    )
    .unwrap();

    let mut res = AstScanResult::default();
    for (rel, path) in [("utils.ts", root.join("utils.ts")), ("app.ts", root.join("app.ts"))] {
        let content = std::fs::read_to_string(path).unwrap();
        ob2h::project::ast_backend::ts::parse_source(&rel, &content, "ts", &mut res).unwrap();
    }
    resolve_calls(&mut res);

    let edge = res.edges.iter().find(|e| {
        e.label == "CALLS"
            && e.source_node_id == "fn:app.ts:mainApp"
            && e.target_node_id == "fn:utils.ts:helper"
    });
    assert!(
        edge.is_some(),
        "ребро mainApp → utils.helper не найдено; рёбра: {:?}",
        res.edges
            .iter()
            .map(|e| format!("{}:{}:{}", e.label, e.source_node_id, e.target_node_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(edge.unwrap().provenance, "RESOLVED");
}

/// 71.6 (виртуальные вызовы): receiver-переменная с единственным одноимённым
/// символом файла — CALLS-ребро provenance=HEURISTIC; несколько кандидатов —
/// AMBIGUOUS (ребра нет).
#[test]
fn virtual_call_single_candidate_heuristic() {
    // Единственный кандидат: run_pipeline вызывает store.fetch, в файле один fetch.
    let src = r#"
pub fn fetch(key: &str) -> &'static str {
    "v"
}

pub fn run_pipeline(store: &dyn Store) {
    let v = store.fetch("k");
}
"#;
    let res = parse_regex("src/v.rs", src);
    let edge = res.edges.iter().find(|e| {
        e.label == "CALLS"
            && e.source_node_id == "fn:src/v.rs:run_pipeline"
            && e.target_node_id == "fn:src/v.rs:fetch"
    });
    assert!(edge.is_some(), "виртуальный вызов не разрешён: {:?}", res.edges);
    assert_eq!(edge.unwrap().provenance, "HEURISTIC");
    assert!(edge.unwrap().context.starts_with("typepass:virtual:"));

    // Два кандидата → AMBIGUOUS: ребра нет.
    let src2 = r#"
pub fn ping() -> i32 {
    1
}

pub fn ping(port: u16) -> i32 {
    2
}

pub fn start() -> i32 {
    let c = Conn {};
    c.ping()
}
"#;
    let res2 = parse_regex("src/amb.rs", src2);
    assert!(
        !res2.edges.iter().any(|e| {
            e.label == "CALLS"
                && e.source_node_id == "fn:src/amb.rs:start"
                && e.target_node_id.contains("ping")
        }),
        "неоднозначный вызов не должен разрешаться; рёбра: {:?}",
        res2.edges
    );
}

/// 71.6 (статика): `Type::method()` с типом того же файла — RESOLVED.
#[test]
fn static_call_resolved() {
    let src = r#"
pub struct TokenCache;

impl TokenCache {
    pub fn refresh(&self) -> bool {
        true
    }
}

pub fn run() -> bool {
    TokenCache::refresh(&TokenCache)
}
"#;
    // диспетчер — продакшн-путь (CST-квалификатор Type::method() под treesitter)
    let mut res = AstScanResult::default();
    ob2h::project::ast_backend::AstParserDispatcher::new().parse_source("src/static.rs", src, &mut res);
    resolve_calls(&mut res);
    let edge = res.edges.iter().find(|e| {
        e.label == "CALLS"
            && e.source_node_id == "fn:src/static.rs:run"
            && e.target_node_id == "fn:src/static.rs:refresh"
    });
    assert!(edge.is_some(), "статический вызов не разрешён: {:?}", res.edges);
    assert_eq!(edge.unwrap().provenance, "RESOLVED");
    // regex-бэкенд: тот же файл, тот же точный путь (typepass:same-file);
    // CST-бэкенд даёт структурно точный контекст typepass:static
    #[cfg(feature = "treesitter")]
    assert!(edge
        .unwrap()
        .context
        .contains("typepass:static:TokenCache::refresh"));
}

/// Go: `pkg.Func()` — символы каталога-пакета (typepass:gopkg, RESOLVED).
#[cfg(feature = "treesitter")]
#[test]
fn import_table_go_package_call() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("helpers")).unwrap();
    std::fs::write(
        root.join("helpers/util.go"),
        "package helpers\n\nfunc Run(n int) int {\n\treturn n + 1\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("main.go"),
        "package main\n\nimport (\n\t\"ob2h/helpers\"\n)\n\nfunc main() {\n\thelpers.Run(1)\n}\n",
    )
    .unwrap();

    let mut res = AstScanResult::default();
    for (rel, path) in [
        ("helpers/util.go", root.join("helpers/util.go")),
        ("main.go", root.join("main.go")),
    ] {
        let content = std::fs::read_to_string(path).unwrap();
        let lang = if rel.ends_with(".go") { "go" } else { "ts" };
        ob2h::project::ast_backend::ts::parse_source(&rel, &content, lang, &mut res).unwrap();
    }
    resolve_calls(&mut res);

    let edge = res.edges.iter().find(|e| {
        e.label == "CALLS"
            && e.source_node_id == "fn:main.go:main"
            && e.target_node_id == "fn:helpers/util.go:Run"
    });
    assert!(
        edge.is_some(),
        "ребро main → helpers.Run не найдено; рёбра: {:?}",
        res.edges
            .iter()
            .map(|e| format!("{}:{}:{}", e.label, e.source_node_id, e.target_node_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(edge.unwrap().provenance, "RESOLVED");
}

// ——— 71.8: co-change граф (CO_CHANGES_WITH, GIT_HISTORY) ———

/// Полный цикл: git-репозиторий со связанной парой файлов → скан проекта →
/// ребро CO_CHANGES_WITH между file:-узлами; повторный скан — идемпотентно.
#[test]
fn cochange_edges_from_git_history() -> anyhow::Result<()> {
    use ob2h::project::ProjectService;

    let dir = tempfile::tempdir()?;
    let root = dir.path();
    std::fs::write(root.join("handler.rs"), "fn handle() {}\n")?;
    std::fs::write(root.join("migration.sql"), "CREATE TABLE t(x);\n")?;
    std::fs::write(root.join("client.dart"), "class Client {}\n")?;
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy()])
            .args(args)
            .output()?;
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok::<(), anyhow::Error>(())
    };
    run(&["init", "-q"])?;
    run(&["config", "user.email", "t@t"])?;
    run(&["config", "user.name", "t"])?;
    for i in 0..3 {
        std::fs::write(root.join("handler.rs"), format!("fn handle() {{ {i} }}\n"))?;
        std::fs::write(root.join("migration.sql"), format!("-- v{i}\nCREATE TABLE t(x);"))?;
        run(&["add", "handler.rs", "migration.sql"])?;
        run(&["commit", "-q", "-m", &format!("c{i}")])?;
    }
    std::fs::write(root.join("client.dart"), "class Client { int x = 1; }\n")?;
    run(&["add", "client.dart"])?;
    run(&["commit", "-q", "-m", "solo"])?;

    let db_path = dir.path().join("t.db");
    let db = ob2h::db::Database::new(&db_path)?;
    let service = ProjectService::new(db.conn_arc());
    service.register_project("co_proj", "CoChange", &root.to_string_lossy(), None, None)?;
    service.scan_project("co_proj", None, false)?;

    let (count, prov, conf): (i64, String, f64) = {
        let arc = db.conn_arc();
        let conn = arc.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(provenance), ''), COALESCE(MAX(confidence), 0.0)
             FROM graph_edges WHERE label = 'CO_CHANGES_WITH'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?
    };
    assert_eq!(count, 1, "одно ребро handler↔migration; client.dart один");
    assert_eq!(prov, "GIT_HISTORY");
    assert!((conf - 0.5).abs() < 1e-9, "confidence по частоте (3 коммита): {conf}");

    // Повторный (полный) скан — идемпотентно: ребро одно, не дублируется
    service.scan_project("co_proj", None, false)?;
    let count2: i64 = {
        let arc = db.conn_arc();
        let conn = arc.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM graph_edges WHERE label = 'CO_CHANGES_WITH'",
            [],
            |r| r.get(0),
        )?
    };
    assert_eq!(count2, 1, "upsert, а не дубликат");

    Ok(())
}

// ——— 71.6: project_impact — TESTED_BY и фильтр confidence ———

/// TESTED_BY: тестовый узел (kind_detail='test'), вызывающий целевой символ
/// через CALLS (confidence 0.5 — эвристика), попадает в affected_tests при
/// дефолтном пороге 0.5 и отфильтровывается при 0.7 (только точные рёбра).
#[test]
fn impact_tested_by_and_confidence_filter() -> anyhow::Result<()> {
    use ob2h::graph::analytics::GraphAnalytics;

    let db = ob2h::db::Database::in_memory()?;
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, root_path, created_at, updated_at)
             VALUES ('t71', 'T71', '/app', '2026-01-01', '2026-01-01')",
            [],
        )?;
        // целевой символ + тест, вызывающий его через эвристическое ребро
        conn.execute(
            "INSERT INTO graph_nodes (node_id, label, node_type, file_path, project_id, provenance, created_at, updated_at)
             VALUES ('fn:src/store.rs:fetch', 'fetch', 'Function', 'src/store.rs', 't71', 'SYNTAX', '2026-01-01', '2026-01-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO graph_nodes (node_id, label, node_type, file_path, project_id, provenance, kind_detail, created_at, updated_at)
             VALUES ('fn:tests/store_test.rs:fetch_test', 'fetch_test', 'Function', 'tests/store_test.rs', 't71', 'SYNTAX', 'test', '2026-01-01', '2026-01-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, weight, contexts, project_id, provenance, confidence, created_at, updated_at)
             SELECT s.id, t.id, 'CALLS', 1.0, 'typepass:virtual:fetch', 't71', 'HEURISTIC', 0.5, '2026-01-01', '2026-01-01'
             FROM graph_nodes s, graph_nodes t
             WHERE s.node_id = 'fn:tests/store_test.rs:fetch_test' AND t.node_id = 'fn:src/store.rs:fetch'",
            [],
        )?;
        Ok(())
    })?;

    // дефолтный порог 0.5: тест виден (эвристика проходит)
    let rep = db.with_conn(|conn| GraphAnalytics::analyze_impact(conn, "t71", "fetch", 3, 0.5))?;
    assert_eq!(rep.affected_nodes.len(), 1, "эвристическое ребро на пороге 0.5 проходит");
    assert_eq!(rep.affected_tests.len(), 1, "TESTED_BY: fetch_test");
    assert_eq!(rep.affected_tests[0].label, "fetch_test");
    assert_eq!(rep.affected_tests[0].file_path.as_deref(), Some("tests/store_test.rs"));
    assert!(rep
        .markdown_summary
        .contains("Затронутые тесты (TESTED_BY)"));

    // порог 0.7: эвристика отфильтрована — ни узла, ни теста
    let strict = db.with_conn(|conn| GraphAnalytics::analyze_impact(conn, "t71", "fetch", 3, 0.7))?;
    assert!(strict.affected_nodes.is_empty(), "эвристика отсечена на 0.7");
    assert!(strict.affected_tests.is_empty());

    Ok(())
}

// ——— 71.5: SFC Vue/Svelte — tree-sitter инъекции `<script>` ———

/// Гибридный разбор .vue: regex-мосты (BINDS_CLASS из template) сохраняются,
/// скрипт-блок разбирается tree-sitter'ом (строки — относительно всего SFC),
/// вызовы из скрипта резолвятся по таблице импортов в helper-файл.
#[cfg(feature = "treesitter")]
#[test]
fn sfc_hybrid_vue_script_and_imports() {
    use ob2h::project::ast_backend::AstParserDispatcher;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("format.ts"),
        "export function formatLabel(s: string): string {\n  return s.trim();\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Card.vue"),
        "<template>\n  <div class=\"card-title\">x</div>\n</template>\n\n<script setup lang=\"ts\">\nimport { formatLabel } from './format';\n\nexport function render(v: string): string {\n  return formatLabel(v);\n}\n</script>\n",
    )
    .unwrap();

    let disp = AstParserDispatcher::new();
    let mut res = AstScanResult::default();
    // helper-файл тоже разбираем — без его File-узла таблице импортов
    // некуда резолвить './format'
    disp.parse_source(
        "format.ts",
        &std::fs::read_to_string(root.join("format.ts")).unwrap(),
        &mut res,
    );
    let backend = disp.parse_source("Card.vue", &std::fs::read_to_string(root.join("Card.vue")).unwrap(), &mut res);

    // под treesitter — гибридный бэкенд; regex-мосты на месте
    assert_eq!(backend, "treesitter:sfc");
    assert!(
        res.edges.iter().any(|e| e.label == "BINDS_CLASS" && e.target_node_id == "cssclass:card-title"),
        "мост BINDS_CLASS из template потерян; рёбра: {:?}",
        res.edges.iter().map(|e| &e.label).collect::<Vec<_>>()
    );
    // символ из скрипта — tree-sitter (строка относительно всего файла:
    // render на 8-й строке SFC)
    let render = res.nodes.iter().find(|n| n.label == "render");
    assert!(render.is_some(), "символ render из <script> не найден");
    assert_eq!(render.unwrap().line_start, 8);

    // вызов formatLabel резолвится по таблице импортов скрипта
    resolve_calls(&mut res);
    let edge = res.edges.iter().find(|e| {
        e.label == "CALLS"
            && e.source_node_id == "fn:Card.vue:render"
            && e.target_node_id == "fn:format.ts:formatLabel"
    });
    assert!(
        edge.is_some(),
        "вызов formatLabel не разрешён; рёбра: {:?}",
        res.edges
            .iter()
            .map(|e| format!("{}:{}:{}", e.label, e.source_node_id, e.target_node_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(edge.unwrap().provenance, "RESOLVED");
}

/// Svelte: тот же гибридный путь (backend treesitter:sfc).
#[cfg(feature = "treesitter")]
#[test]
fn sfc_hybrid_svelte() {
    use ob2h::project::ast_backend::AstParserDispatcher;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Widget.svelte"),
        "<script lang=\"ts\">\nexport function widgetFn(): number {\n  return 42;\n}\n</script>\n\n<h1>hi</h1>\n",
    )
    .unwrap();

    let disp = AstParserDispatcher::new();
    let mut res = AstScanResult::default();
    let backend = disp.parse_source(
        "Widget.svelte",
        &std::fs::read_to_string(root.join("Widget.svelte")).unwrap(),
        &mut res,
    );
    assert_eq!(backend, "treesitter:sfc");
    let f = res.nodes.iter().find(|n| n.label == "widgetFn");
    assert!(f.is_some(), "символ widgetFn не найден");
    assert_eq!(f.unwrap().line_start, 2);
}

/// Без фичи treesitter SFC идёт регулярным путём (v1.x поведение).
#[cfg(not(feature = "treesitter"))]
#[test]
fn sfc_regex_fallback() {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    let backend = ob2h::project::ast_backend::AstParserDispatcher::new()
        .parse_source("Card.vue", "<script>\nexport function render() {}\n</script>\n", &mut res);
    assert_eq!(backend, "regex");
    assert!(res.nodes.iter().any(|n| n.label == "render"));
}

// ——— 71.7: F1-метрики golden — вспомогательные функции ———

/// Достать SYNTAX-символы (словарь SYMBOL_TYPES) как (label, node_type).
pub(crate) fn symbol_set(scan: &AstScanResult) -> HashSet<(String, String)> {
    use ob2h::graph::callpath::SYMBOL_TYPES;
    scan.nodes
        .iter()
        .filter(|n| SYMBOL_TYPES.contains(&n.node_type.as_str()))
        .map(|n| (n.label.clone(), n.node_type.clone()))
        .collect()
}

/// Пробник 71.7 (запуск вручную: cargo test probe_golden_f1 -- --nocapture):
/// дамп фактических символов и не-эвристических CALLS по каждому эталону —
/// для сопровождения исчерпывающих expected.json.
#[test]
#[ignore = "пробник: cargo test --test test_phase71 probe_golden_f1 -- --ignored --nocapture"]
fn probe_golden_f1() {
    let items = ob2h::project::ast_golden::load_all(&fixtures_dir()).unwrap();
    for (path, g) in &items {
        let src = std::fs::read_to_string(path).unwrap();
        let res = parse_regex(&g.fixture, &src);
        let mut syms: Vec<String> = symbol_set(&res)
            .into_iter()
            .map(|(l, k)| format!("{k} {l}"))
            .collect();
        syms.sort();
        println!("=== {} symbols: {:#?}", g.language, syms);
        let mut calls: Vec<String> = res
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.provenance != "HEURISTIC")
            .map(|e| {
                let from = res
                    .nodes
                    .iter()
                    .find(|n| n.node_id == e.source_node_id)
                    .map(|n| n.label.clone())
                    .unwrap_or_default();
                let to = res
                    .nodes
                    .iter()
                    .find(|n| n.node_id == e.target_node_id)
                    .map(|n| n.label.clone())
                    .unwrap_or_default();
                format!("{from} -> {to} [{} {}]", e.provenance, e.context)
            })
            .collect();
        calls.sort();
        println!("=== {} calls: {:#?}\n", g.language, calls);
    }
}
