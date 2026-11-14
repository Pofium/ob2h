//! Ф70.1 (PLAN_v2.0): smoke — грамматики tree-sitter загружаются и совместимы
//! с ABI ядра 0.27 (проверка LANGUAGE_VERSION на set_language), минимальный
//! разбор без ERROR-узлов. Запуск: `cargo test --features treesitter`.
#![cfg(feature = "treesitter")]

use tree_sitter::Parser;

/// (имя, грамматика, минимальный корректный сниппет).
const LANGS: &[(&str, fn() -> tree_sitter::Language, &str)] = &[
    ("rust", || tree_sitter_rust::LANGUAGE.into(), "pub fn main() {}"),
    (
        "python",
        || tree_sitter_python::LANGUAGE.into(),
        "def main():\n    pass\n",
    ),
    (
        "ts",
        || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "export function main(): void {}\n",
    ),
    (
        "go",
        || tree_sitter_go::LANGUAGE.into(),
        "package main\n\nfunc main() {}\n",
    ),
    (
        "dart",
        || tree_sitter_dart::LANGUAGE.into(),
        "void main() {}\n",
    ),
];

#[test]
fn grammars_load_and_parse_without_errors() {
    for (name, lang, snippet) in LANGS {
        let mut parser = Parser::new();
        let lang = lang();
        parser
            .set_language(&lang)
            .unwrap_or_else(|e| panic!("{name}: set_language ABI-несовместим: {e}"));
        let tree = parser
            .parse(*snippet, None)
            .unwrap_or_else(|| panic!("{name}: parser.parse вернул None"));
        let root = tree.root_node();
        assert!(
            !root.has_error(),
            "{name}: корневое дерево содержит ERROR на минимальном сниппете:\n{snippet}"
        );
        assert!(root.child_count() > 0, "{name}: пустое дерево");
    }
}

#[test]
fn grammar_budget_is_tracked() {
    // Ф70.11 (заготовка): 5 грамматик суммарно ≤ 8 МБ в бинарнике.
    // Здесь — инвариант набора: ровно 5 поддерживаемых языков.
    assert_eq!(LANGS.len(), 5, "топ-5: rust, python, ts, go, dart");
}

// ——— Ф70.3: Rust mapper (CST → AstScanResult) ———

use std::collections::HashSet;

use ob2h::project::ast::{AstCodeExtractor, AstScanResult};
use ob2h::project::ast_backend::{ts, AstParserDispatcher};

fn map_rust_golden() -> AstScanResult {
    let golden = ob2h::project::ast_golden::load_embedded()
        .expect("вшитые эталоны")
        .into_iter()
        .find(|g| g.spec.language == "rust")
        .expect("эталон rust");
    let mut out = AstScanResult::default();
    ts::parse_source("source.rs", &golden.source, "rust", &mut out)
        .expect("rust mapper не должен отказывать на эталоне");
    out
}

/// Эталонные символы и вызовы найдены tree-sitter mapper'ом (гейт 70.11/71.7).
#[test]
fn rust_mapper_meets_golden() {
    let res = map_rust_golden();
    for (name, kind) in [
        ("CacheConfig", "Struct"),
        ("TokenStore", "Trait"),
        ("build_cache", "Function"),
        ("run_pipeline", "Function"),
    ] {
        assert!(
            res.nodes
                .iter()
                .any(|n| n.label == name && n.node_type == kind),
            "символ {name} ({kind}) не найден; узлы: {:?}",
            res.nodes
                .iter()
                .map(|n| format!("{}:{}", n.node_type, n.label))
                .collect::<Vec<_>>()
        );
    }
    // CALLS-ребро type-pass: run_pipeline → build_cache (resolve_calls на выходе mapper'а
    // выполняет диспетчер скана; здесь — напрямую).
    let mut res = map_rust_golden();
    let n = ob2h::project::ast::resolve_calls(&mut res);
    assert!(n > 0, "type-pass не дал рёбер");
    let from = res
        .nodes
        .iter()
        .find(|n| n.label == "run_pipeline")
        .map(|n| n.node_id.clone())
        .unwrap();
    let to = res
        .nodes
        .iter()
        .find(|n| n.label == "build_cache")
        .map(|n| n.node_id.clone())
        .unwrap();
    assert!(
        res.edges
            .iter()
            .any(|e| e.source_node_id == from && e.target_node_id == to && e.label == "CALLS"),
        "нет CALLS run_pipeline → build_cache"
    );
}

/// Паритет node_id с regex-бэкендом на эталоне (привязка памяти стабильна).
#[test]
fn rust_mapper_node_ids_match_regex() {
    let golden = ob2h::project::ast_golden::load_embedded()
        .unwrap()
        .into_iter()
        .find(|g| g.spec.language == "rust")
        .unwrap();
    let mut regex_res = AstScanResult::default();
    AstCodeExtractor::new().parse_file("source.rs", &golden.source, &mut regex_res);

    let ts_res = map_rust_golden();
    let ids_regex: HashSet<&str> = regex_res
        .nodes
        .iter()
        .map(|n| n.node_id.as_str())
        .collect();
    let ids_ts: HashSet<&str> = ts_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(
        ids_regex, ids_ts,
        "node_id tree-sitter и regex разошлись: ts-нет: {:?}, regex-нет: {:?}",
        ids_ts.difference(&ids_regex).collect::<Vec<_>>(),
        ids_regex.difference(&ids_ts).collect::<Vec<_>>(),
    );
}

/// Superset tree-sitter: методы impl-блоков и сигнатуры трейтов — узлы,
/// которые regex-бэкенд терял; плюс ROUTE от #[get(...)].
#[test]
fn rust_mapper_impl_methods_and_route() {
    let src = r#"use axum::routing::get;

pub struct Api;

pub trait Repo {
    fn get(&self, id: u64) -> Option<String>;
}

impl Api {
    pub fn health(&self) -> &'static str {
        "ok"
    }
}

#[get("/users/:id")]
pub async fn user_by_id() -> &'static str {
    "u"
}
"#;
    let mut out = AstScanResult::default();
    ts::parse_source("api.rs", src, "rust", &mut out).expect("разбор");

    // impl-метод → Function node (regex-бэкенд это терял)
    assert!(
        out.nodes
            .iter()
            .any(|n| n.label == "health" && n.node_type == "Function"),
        "метод impl не найден"
    );
    // сигнатура трейта → Function node
    assert!(
        out.nodes
            .iter()
            .any(|n| n.label == "get" && n.node_type == "Function"),
        "сигнатура трейта не найдена"
    );
    // ROUTE от #[get("/users/:id")] — ребро от fn user_by_id
    let fn_id = out
        .nodes
        .iter()
        .find(|n| n.label == "user_by_id")
        .map(|n| n.node_id.clone())
        .unwrap();
    assert!(
        out.edges
            .iter()
            .any(|e| e.source_node_id == fn_id && e.label == "ROUTE" && e.target_node_id.contains("/users/:id")),
        "ROUTE-ребро не найдено; рёбра: {:?}",
        out.edges
            .iter()
            .map(|e| format!("{}:{}:{}->{}", e.provenance, e.label, e.source_node_id, e.target_node_id))
            .collect::<Vec<_>>()
    );
}

/// Детерминизм mapper'а: двойной разбор → то же множество node_id.
#[test]
fn rust_mapper_deterministic() {
    let a = map_rust_golden();
    let b = map_rust_golden();
    let ia: HashSet<&str> = a.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let ib: HashSet<&str> = b.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(ia, ib);
    let ea: HashSet<String> = a
        .edges
        .iter()
        .map(|e| format!("{}|{}|{}", e.source_node_id, e.target_node_id, e.label))
        .collect();
    let eb: HashSet<String> = b
        .edges
        .iter()
        .map(|e| format!("{}|{}|{}", e.source_node_id, e.target_node_id, e.label))
        .collect();
    assert_eq!(ea, eb);
}

/// Диспетчер под фичей: .rs уходит в treesitter:rust и проходит эталон.
#[test]
fn dispatcher_routes_rust_to_treesitter() {
    let golden = ob2h::project::ast_golden::load_embedded()
        .unwrap()
        .into_iter()
        .find(|g| g.spec.language == "rust")
        .unwrap();
    let disp = AstParserDispatcher::new();
    let mut out = AstScanResult::default();
    let backend = disp.parse_source("source.rs", &golden.source, &mut out);
    assert_eq!(backend, "treesitter:rust");
    assert!(
        out.nodes
            .iter()
            .any(|n| n.label == "run_pipeline" && n.node_type == "Function"),
        "эталонный fn не найден через диспетчер"
    );
}
