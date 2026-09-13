//! Ф36 (трек C, PLAN_v1.4): structural queries — call-path, callers/callees,
//! dead-code. Граф севится напрямую в graph_nodes/graph_edges (детерминированный
//! фикстур; эквивалент сканирования fixture-репо на уровне модели данных).

use ob2h::db::Database;
use ob2h::graph::callpath::{self, Dir};
use rusqlite::params;

const PROJ: &str = "cp";

/// Фикстур: main → app → parser → lexer, плюс util (мёртвый), pub_api (public),
/// тестовая fn и helper (вызывается из теста).
fn seed(db: &Database) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, root_path, created_at, updated_at) \
             VALUES (?1, 'CallPath', '/tmp/cp', '2026-01-01', '2026-01-01')",
            params![PROJ],
        )?;

        let nodes: [(&str, &str, &str, Option<&str>); 8] = [
            ("n-main", "main", "Function", Some("src/main.rs")),
            ("n-app", "run_app", "Function", Some("src/app.rs")),
            ("n-parser", "parse", "Function", Some("src/parser.rs")),
            ("n-lexer", "lex", "Function", Some("src/lexer.rs")),
            ("n-util", "util_dead", "Function", Some("src/util.rs")),
            ("n-api", "api_open", "Function", Some("src/api.rs")),
            ("n-test", "test_helper", "Function", Some("tests/helper.rs")),
            ("n-struct", "Config", "Struct", Some("src/config.rs")),
        ];
        for (i, (nid, label, ntype, file)) in nodes.iter().enumerate() {
            let desc = if *nid == "n-api" {
                "Функция `api_open` (pub async fn api_open() {) в src/api.rs"
            } else {
                "Функция `x` (fn x() {) в src/x.rs"
            };
            conn.execute(
                "INSERT INTO graph_nodes (id, node_id, label, node_type, description, file_path, line_start, project_id, provenance, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 'ast', '2026-01-01', '2026-01-01')",
                params![(i + 1) as i64, nid, label, ntype, desc, file, PROJ],
            )?;
        }

        // рёбра использования: main→app→parser→lexer; тест вызывает helper;
        // Config описан pub-структурой, но никем не используется (мёртвый)
        let edges: [(i64, i64, &str); 4] = [
            (1, 2, "CALLS"),
            (2, 3, "CALLS"),
            (3, 4, "CALLS"),
            (7, 7, "CALLS"), // self-loop в тесте (не влияет на dead других)
        ];
        for (s, t, label) in edges {
            conn.execute(
                "INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'ast', '2026-01-01')",
                params![s, t, label, PROJ],
            )?;
        }
        Ok(())
    })
    .expect("seed");
}

/// 36.1: цепочка A→B→C находится; path от main к lex — 3 шага через нужные рёбра.
#[test]
fn call_path_finds_chain() {
    let db = Database::in_memory().expect("db");
    seed(&db);
    db.with_conn(|conn| {
        let from = callpath::resolve_symbol(conn, PROJ, "main").expect("resolve").expect("main");
        let to = callpath::resolve_symbol(conn, PROJ, "lex").expect("resolve").expect("lex");
        let path = callpath::call_path(conn, PROJ, &from, &to, 5).expect("path").expect("путь найден");
        assert_eq!(path.hops, 3, "main→app→parser→lex: 3 шага");
        let labels: Vec<&str> = path.chain.iter().map(|l| l.symbol.label.as_str()).collect();
        assert_eq!(labels, vec!["run_app", "parse", "lex"]);
        assert!(path.chain.iter().all(|l| l.edge == "CALLS"));

        // пути к мёртвому util нет
        let util = callpath::resolve_symbol(conn, PROJ, "util_dead").unwrap().unwrap();
        assert!(callpath::call_path(conn, PROJ, &from, &util, 5).unwrap().is_none());
        Ok(())
    })
    .expect("call_path");
}

/// 36.1/36.2: callers/callees на глубину; у main нет callers, у lex — один caller.
#[test]
fn callers_and_callees_bfs() {
    let db = Database::in_memory().expect("db");
    seed(&db);
    db.with_conn(|conn| {
        let main = callpath::resolve_symbol(conn, PROJ, "main").unwrap().unwrap();
        let callees = callpath::neighbors(conn, PROJ, main.id, Dir::Callees, 3, 25).unwrap();
        let names: Vec<&str> = callees.iter().map(|l| l.symbol.label.as_str()).collect();
        assert_eq!(names, vec!["run_app", "parse", "lex"], "callees BFS по глубинам");
        assert_eq!(callees[0].depth, 1);
        assert_eq!(callees[2].depth, 3);

        let callers = callpath::neighbors(conn, PROJ, main.id, Dir::Callers, 3, 25).unwrap();
        assert!(callers.is_empty(), "у main нет callers");

        let lex = callpath::resolve_symbol(conn, PROJ, "lex").unwrap().unwrap();
        // глубина 1 — только прямые callers
        let direct = callpath::neighbors(conn, PROJ, lex.id, Dir::Callers, 1, 25).unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].symbol.label, "parse");
        // глубина 3 — вся цепочка вверх с глубинами
        let all = callpath::neighbors(conn, PROJ, lex.id, Dir::Callers, 3, 25).unwrap();
        let names: Vec<&str> = all.iter().map(|l| l.symbol.label.as_str()).collect();
        assert_eq!(names, vec!["parse", "run_app", "main"], "BFS callers по глубинам");
        let depths: Vec<usize> = all.iter().map(|l| l.depth).collect();
        assert_eq!(depths, vec![1, 2, 3]);
        Ok(())
    })
    .expect("bfs");
}

/// 36.3: мёртвый символ в отчёте; entrypoints (main, тесты, pub API) — нет.
#[test]
fn dead_code_excludes_entrypoints() {
    let db = Database::in_memory().expect("db");
    seed(&db);
    db.with_conn(|conn| {
        let dead = callpath::dead_code(conn, PROJ).expect("dead");
        let labels: Vec<&str> = dead.iter().map(|d| d.symbol.label.as_str()).collect();

        assert!(labels.contains(&"util_dead"), "мёртвый util в отчёте: {labels:?}");
        assert!(labels.contains(&"Config"), "неиспользуемая структура в отчёте");
        assert!(!labels.contains(&"main"), "entrypoint main исключён");
        assert!(!labels.contains(&"run_app"), "используемый символ не мёртв");
        assert!(!labels.contains(&"api_open"), "pub API исключён");
        assert!(!labels.contains(&"test_helper"), "тестовый код исключён");
        Ok(())
    })
    .expect("dead_code");
}

/// 36.3: секция dead-code появляется в project_report.
#[test]
fn project_report_contains_dead_code_section() {
    let db = Database::in_memory().expect("db");
    seed(&db);
    let report = db
        .with_conn(|conn| ob2h::graph::GraphAnalytics::generate_project_report(conn, PROJ))
        .expect("report");
    assert!(
        report.markdown_summary.contains("мёртвый код"),
        "в отчёте нет секции dead-code:\n{}",
        report.markdown_summary
    );
    assert!(report.markdown_summary.contains("util_dead"));
}

/// 36.2: formatting helpers не падают на пустых результатах.
#[test]
fn formatting_is_total() {
    assert!(callpath::format_links("Callers `x` (f:1)", &[]).contains("ничего не найдено"));
    assert!(callpath::format_dead_code(&[], 10).contains("не найдено"));
}
