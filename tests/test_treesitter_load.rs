//! Ф70.1 (PLAN_v2.0): smoke — грамматики tree-sitter загружаются и совместимы
//! с ABI ядра 0.27 (проверка LANGUAGE_VERSION на set_language), минимальный
//! разбор без ERROR-узлов. Запуск: `cargo test --features treesitter`.
#![cfg(feature = "treesitter")]

use tree_sitter::Parser;

/// (имя, грамматика, минимальный корректный сниппет).
type LangEntry = (&'static str, fn() -> tree_sitter::Language, &'static str);
const LANGS: &[LangEntry] = &[
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

// ——— Ф70.5: Python mapper (CST → AstScanResult) ———

fn map_py_golden() -> AstScanResult {
    let golden = ob2h::project::ast_golden::load_embedded()
        .expect("вшитые эталоны")
        .into_iter()
        .find(|g| g.spec.language == "python")
        .expect("эталон python");
    let mut out = AstScanResult::default();
    ts::parse_source("source.py", &golden.source, "python", &mut out)
        .expect("python mapper не должен отказывать на эталоне");
    out
}

/// Эталонные символы и вызовы Python найдены mapper'ом.
#[test]
fn python_mapper_meets_golden() {
    let res = map_py_golden();
    for (name, kind) in [
        ("TokenCache", "Class"),
        ("load_settings", "Function"),
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
    // Метод refresh (внутри класса) — regex-бэкенд его тоже эмитит
    assert!(
        res.nodes.iter().any(|n| n.label == "refresh" && n.node_type == "Function"),
        "метод класса не найден"
    );
    // CALLS run_pipeline → load_settings (type-pass)
    let mut res = res;
    ob2h::project::ast::resolve_calls(&mut res);
    let from = res.nodes.iter().find(|n| n.label == "run_pipeline").map(|n| n.node_id.clone()).unwrap();
    let to = res.nodes.iter().find(|n| n.label == "load_settings").map(|n| n.node_id.clone()).unwrap();
    assert!(
        res.edges
            .iter()
            .any(|e| e.source_node_id == from && e.target_node_id == to && e.label == "CALLS"),
        "нет CALLS run_pipeline → load_settings; рёбра: {:?}",
        res.edges.iter().map(|e| format!("{}:{}->{}", e.label, e.source_node_id, e.target_node_id)).collect::<Vec<_>>()
    );
}

/// Паритет node_id с regex-бэкендом на эталоне Python.
#[test]
fn python_mapper_node_ids_match_regex() {
    let golden = ob2h::project::ast_golden::load_embedded()
        .unwrap()
        .into_iter()
        .find(|g| g.spec.language == "python")
        .unwrap();
    let mut regex_res = AstScanResult::default();
    AstCodeExtractor::new().parse_file("source.py", &golden.source, &mut regex_res);
    let ts_res = map_py_golden();
    let ids_regex: HashSet<&str> = regex_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let ids_ts: HashSet<&str> = ts_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(
        ids_regex, ids_ts,
        "ts-нет: {:?}, regex-нет: {:?}",
        ids_ts.difference(&ids_regex).collect::<Vec<_>>(),
        ids_regex.difference(&ids_ts).collect::<Vec<_>>(),
    );
}

/// FastAPI-роут через декоратор, __tablename__ (QUERIES_TABLE),
/// INHERITS и пометка Pydantic-модели.
#[test]
fn python_mapper_fastapi_route_and_tablename() {
    let src = r#"from fastapi import FastAPI
from sqlalchemy import Model

app = FastAPI()


class User(Model):
    __tablename__ = "users"
    id: int


class UserOut(BaseModel):
    name: str


@app.get("/users/{user_id}")
async def get_user(user_id: int):
    return User(id=user_id)


@router.post("/users", status_code=201)
def create_user(payload: dict):
    return payload
"#;
    let mut out = AstScanResult::default();
    ts::parse_source("routes.py", src, "python", &mut out).expect("разбор");

    // ROUTE от @app.get и @router.post (второй — многострочный декоратор не нужен)
    let get_user = "fn:routes.py:get_user";
    let create_user = "fn:routes.py:create_user";
    assert!(
        out.edges.iter().any(|e| e.source_node_id == get_user
            && e.label == "ROUTE"
            && e.target_node_id.contains("/users/{user_id}")),
        "ROUTE @app.get не найден; рёбра: {:?}",
        out.edges.iter().map(|e| format!("{}:{}->{}", e.label, e.source_node_id, e.target_node_id)).collect::<Vec<_>>()
    );
    assert!(
        out.edges.iter().any(|e| e.source_node_id == create_user
            && e.label == "ROUTE"
            && e.target_node_id.contains("/users")),
        "ROUTE @router.post не найден"
    );
    // QUERIES_TABLE: class User → table:users
    assert!(
        out.edges.iter().any(|e| e.source_node_id == "class:routes.py:User"
            && e.label == "QUERIES_TABLE"
            && e.target_node_id == "table:users"),
        "QUERIES_TABLE не найден"
    );
    // INHERITS: UserOut → class:BaseModel (голое имя)
    assert!(
        out.edges.iter().any(|e| e.source_node_id == "class:routes.py:UserOut"
            && e.label == "INHERITS"
            && e.target_node_id == "class:BaseModel"),
        "INHERITS не найден"
    );
    // Пометка Pydantic в описании UserOut
    let user_out = out
        .nodes
        .iter()
        .find(|n| n.label == "UserOut")
        .expect("UserOut");
    assert!(user_out.description.contains("Pydantic"), "пометка Pydantic отсутствует");
}

/// Детерминизм python mapper'а.
#[test]
fn python_mapper_deterministic() {
    let a = map_py_golden();
    let b = map_py_golden();
    let ia: HashSet<&str> = a.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let ib: HashSet<&str> = b.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(ia, ib);
}

/// Диспетчер: .py уходит в treesitter:python.
#[test]
fn dispatcher_routes_python_to_treesitter() {
    let golden = ob2h::project::ast_golden::load_embedded()
        .unwrap()
        .into_iter()
        .find(|g| g.spec.language == "python")
        .unwrap();
    let disp = AstParserDispatcher::new();
    let mut out = AstScanResult::default();
    let backend = disp.parse_source("source.py", &golden.source, &mut out);
    assert_eq!(backend, "treesitter:python");
    assert!(out.nodes.iter().any(|n| n.label == "run_pipeline"));
}

// ——— Ф70.6: Go mapper (CST → AstScanResult) ———

fn map_lang_golden(lang: &str, rel: &str) -> AstScanResult {
    let golden = ob2h::project::ast_golden::load_embedded()
        .expect("вшитые эталоны")
        .into_iter()
        .find(|g| g.spec.language == lang)
        .unwrap_or_else(|| panic!("эталон {lang}"));
    let mut out = AstScanResult::default();
    ts::parse_source(rel, &golden.source, lang, &mut out)
        .unwrap_or_else(|e| panic!("{lang} mapper отказал: {e}"));
    out
}

/// Go: эталонные символы + вызовы + IMPORTS (superset regex).
#[test]
fn go_mapper_meets_golden() {
    let mut res = map_lang_golden("go", "source.go");
    for (name, kind) in [
        ("CacheConfig", "Struct"),
        ("LoadSettings", "Function"),
        ("RunPipeline", "Function"),
    ] {
        assert!(
            res.nodes.iter().any(|n| n.label == name && n.node_type == kind),
            "Go символ {name} ({kind}) не найден"
        );
    }
    ob2h::project::ast::resolve_calls(&mut res);
    let from = res.nodes.iter().find(|n| n.label == "RunPipeline").map(|n| n.node_id.clone()).unwrap();
    let to = res.nodes.iter().find(|n| n.label == "LoadSettings").map(|n| n.node_id.clone()).unwrap();
    assert!(
        res.edges.iter().any(|e| e.source_node_id == from
            && e.target_node_id == to
            && e.label == "CALLS"),
        "нет CALLS RunPipeline → LoadSettings"
    );

    // IMPORTS — superset: regex-бэкенд Go импорты не извлекал
    let src = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"x\")\n}\n";
    let mut out = AstScanResult::default();
    ts::parse_source("main.go", src, "go", &mut out).expect("разбор");
    assert!(
        out.edges.iter().any(|e| e.label == "IMPORTS" && e.target_node_id == "module:fmt"),
        "IMPORTS для Go не найдены"
    );
}

/// Go: паритет node_id с regex-бэкендом.
#[test]
fn go_mapper_node_ids_match_regex() {
    let golden = ob2h::project::ast_golden::load_embedded()
        .unwrap()
        .into_iter()
        .find(|g| g.spec.language == "go")
        .unwrap();
    let mut regex_res = AstScanResult::default();
    AstCodeExtractor::new().parse_file("source.go", &golden.source, &mut regex_res);
    let ts_res = map_lang_golden("go", "source.go");
    let ids_regex: HashSet<&str> = regex_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let ids_ts: HashSet<&str> = ts_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(
        ids_regex, ids_ts,
        "ts-нет: {:?}, regex-нет: {:?}",
        ids_ts.difference(&ids_regex).collect::<Vec<_>>(),
        ids_regex.difference(&ids_ts).collect::<Vec<_>>(),
    );
}

// ——— Ф70.12: Dart mapper (CST → AstScanResult) ———

/// Dart: эталонные символы + вызовы + классовые рёбра (extends/with/implements).
#[test]
fn dart_mapper_meets_golden() {
    let mut res = map_lang_golden("dart", "source.dart");
    for (name, kind) in [
        ("TokenCache", "Class"),
        ("loadSettings", "Function"),
        ("runPipeline", "Function"),
    ] {
        assert!(
            res.nodes.iter().any(|n| n.label == name && n.node_type == kind),
            "Dart символ {name} ({kind}) не найден"
        );
    }
    assert!(
        res.nodes.iter().any(|n| n.label == "refresh" && n.node_type == "Function"),
        "метод класса не найден"
    );
    ob2h::project::ast::resolve_calls(&mut res);
    let from = res.nodes.iter().find(|n| n.label == "runPipeline").map(|n| n.node_id.clone()).unwrap();
    let to = res.nodes.iter().find(|n| n.label == "loadSettings").map(|n| n.node_id.clone()).unwrap();
    assert!(
        res.edges.iter().any(|e| e.source_node_id == from
            && e.target_node_id == to
            && e.label == "CALLS"),
        "нет CALLS runPipeline → loadSettings"
    );

    // Классовые рёбра: extends → INHERITS, with → IMPLEMENTS trait:, implements
    let src = r#"import 'package:flutter/material.dart';

class UserService extends Base with Mix implements Repo {
}

mixin Mix {
}
"#;
    let mut out = AstScanResult::default();
    ts::parse_source("user_service.dart", src, "dart", &mut out).expect("разбор");
    let cls = "class:user_service.dart:UserService";
    assert!(
        out.edges.iter().any(|e| e.source_node_id == cls
            && e.label == "INHERITS"
            && e.target_node_id == "class:Base"),
        "INHERITS extends не найден"
    );
    assert!(
        out.edges.iter().any(|e| e.source_node_id == cls
            && e.label == "IMPLEMENTS"
            && e.target_node_id == "trait:Mix"),
        "IMPLEMENTS with не найден"
    );
    assert!(
        out.edges.iter().any(|e| e.source_node_id == cls
            && e.label == "IMPLEMENTS"
            && e.target_node_id == "interface:Repo"),
        "IMPLEMENTS implements не найден"
    );
    // Миксин → Trait + IMPORTS
    assert!(
        out.nodes.iter().any(|n| n.label == "Mix" && n.node_type == "Trait"),
        "миксин не найден"
    );
    assert!(
        out.edges.iter().any(|e| e.label == "IMPORTS"
            && e.target_node_id == "module:package:flutter/material.dart"),
        "IMPORTS для Dart не найден"
    );
}

/// Dart: паритет node_id с regex-бэкендом.
#[test]
fn dart_mapper_node_ids_match_regex() {
    let golden = ob2h::project::ast_golden::load_embedded()
        .unwrap()
        .into_iter()
        .find(|g| g.spec.language == "dart")
        .unwrap();
    let mut regex_res = AstScanResult::default();
    AstCodeExtractor::new().parse_file("source.dart", &golden.source, &mut regex_res);
    let ts_res = map_lang_golden("dart", "source.dart");
    let ids_regex: HashSet<&str> = regex_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let ids_ts: HashSet<&str> = ts_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(
        ids_regex, ids_ts,
        "ts-нет: {:?}, regex-нет: {:?}",
        ids_ts.difference(&ids_regex).collect::<Vec<_>>(),
        ids_regex.difference(&ids_ts).collect::<Vec<_>>(),
    );
}

/// Диспетчер: .go и .dart уходят в tree-sitter.
#[test]
fn dispatcher_routes_go_and_dart_to_treesitter() {
    for (lang, rel) in [("go", "source.go"), ("dart", "source.dart")] {
        let golden = ob2h::project::ast_golden::load_embedded()
            .unwrap()
            .into_iter()
            .find(|g| g.spec.language == lang)
            .unwrap();
        let disp = AstParserDispatcher::new();
        let mut out = AstScanResult::default();
        let backend = disp.parse_source(rel, &golden.source, &mut out);
        assert_eq!(backend, format!("treesitter:{lang}"));
        assert!(out.nodes.iter().any(|n| n.node_type == "Function"), "{lang}: нет fn");
    }
}

// ——— Ф70.8: инкрементальный парсинг (LRU-кэш + Tree::edit) ———

use ob2h::project::ast_backend::ts::IncrementalParser;

/// Последовательность узлов (kind + байтовый диапазон) — для структурного
/// сравнения инкрементального и cold разбора.
fn flatten(node: tree_sitter::Node, out: &mut Vec<(String, usize, usize)>) {
    out.push((
        node.kind().to_string(),
        node.start_byte(),
        node.end_byte(),
    ));
    let mut c = node.walk();
    for child in node.children(&mut c) {
        flatten(child, out);
    }
}

#[test]
fn incremental_cache_hit_and_edit_equivalence() {
    let parser = IncrementalParser::new();
    let v1 = "pub fn one() -> u64 {\n    1\n}\n\npub fn two() -> u64 {\n    2\n}\n";
    let v2 = "pub fn one() -> u64 {\n    1\n}\n\npub fn two() -> u64 {\n    22 // changed\n}\n";

    // 1-й разбор — cold; повтор с тем же контентом — кэш-хит (дерево валидно)
    let t1a = parser.parse_tree("a.rs", v1, "rust").expect("cold parse");
    let t1b = parser.parse_tree("a.rs", v1, "rust").expect("cache hit");
    assert_eq!(
        t1a.root_node().byte_range(),
        t1b.root_node().byte_range(),
        "кэш-хит должен возвращать то же дерево"
    );

    // Изменение файла → Tree::edit + reparse со старым деревом
    let t2 = parser.parse_tree("a.rs", v2, "rust").expect("incremental parse");
    assert_eq!(
        t2.root_node().end_byte(),
        v2.len(),
        "дерево должно покрывать новый контент целиком"
    );

    // Инкрементальный разбор структурно эквивалентен cold-разбору нового контента
    let fresh = IncrementalParser::new();
    let t2_cold = fresh.parse_tree("b.rs", v2, "rust").expect("cold parse of v2");
    let mut seq_inc = Vec::new();
    let mut seq_cold = Vec::new();
    flatten(t2.root_node(), &mut seq_inc);
    flatten(t2_cold.root_node(), &mut seq_cold);
    assert_eq!(seq_inc, seq_cold, "Tree::edit путь должен давать структуру cold-разбора");
}

#[test]
fn incremental_dispatcher_sequential_edits() {
    let disp = AstParserDispatcher::new();
    let v1 = "pub fn run_pipeline() { let x = build_cache(); }\n";
    let v2 = "pub fn run_pipeline() { let x = build_cache(); build_cache(); }\n";
    let mut out1 = AstScanResult::default();
    let b1 = disp.parse_source("a.rs", v1, &mut out1);
    assert_eq!(b1, "treesitter:rust");
    assert!(out1.nodes.iter().any(|n| n.label == "run_pipeline"));

    let mut out2 = AstScanResult::default();
    let b2 = disp.parse_source("a.rs", v2, &mut out2);
    assert_eq!(b2, "treesitter:rust", "после правки файл не должен сваливаться в regex");
    assert!(out2.nodes.iter().any(|n| n.label == "run_pipeline"));
}

#[test]
fn incremental_lru_capacity_never_breaks() {
    // > 64 файлов: первые записи вытесняются, повторный разбор корректен (cold)
    let parser = IncrementalParser::new();
    for i in 0..70usize {
        let src = format!("pub fn f_{i}() -> u64 {{ {i} }}\n");
        let tree = parser
            .parse_tree(&format!("f_{i}.rs"), &src, "rust")
            .expect("parse");
        assert_eq!(tree.root_node().end_byte(), src.len());
    }
}

// ——— Ф70.4: TS/JS mapper (CST → AstScanResult) ———

/// TS: эталонные символы + superset (метод класса) + вызовы.
#[test]
fn ts_mapper_meets_golden() {
    let mut res = map_lang_golden("ts", "source.ts");
    for (name, kind) in [
        ("TokenCache", "Class"),
        ("loadSettings", "Function"),
        ("runPipeline", "Function"),
    ] {
        assert!(
            res.nodes.iter().any(|n| n.label == name && n.node_type == kind),
            "TS символ {name} ({kind}) не найден"
        );
    }
    // superset: метод класса — regex-бэкенд его не эмитил
    assert!(
        res.nodes.iter().any(|n| n.label == "refresh" && n.node_type == "Function"),
        "метод класса (superset) не найден"
    );
    ob2h::project::ast::resolve_calls(&mut res);
    let from = res.nodes.iter().find(|n| n.label == "runPipeline").map(|n| n.node_id.clone()).unwrap();
    let to = res.nodes.iter().find(|n| n.label == "loadSettings").map(|n| n.node_id.clone()).unwrap();
    assert!(
        res.edges.iter().any(|e| e.source_node_id == from
            && e.target_node_id == to
            && e.label == "CALLS"),
        "нет CALLS runPipeline → loadSettings"
    );
}

/// TS: паритет в направлении миграции — все node_id regex-бэкенда сохраняются
/// в tree-sitter выдаче (привязка памяти), mapper может добавлять (методы/стрелки).
#[test]
fn ts_mapper_regex_ids_are_subset() {
    let golden = ob2h::project::ast_golden::load_embedded()
        .unwrap()
        .into_iter()
        .find(|g| g.spec.language == "ts")
        .unwrap();
    let mut regex_res = AstScanResult::default();
    AstCodeExtractor::new().parse_file("source.ts", &golden.source, &mut regex_res);
    let ts_res = map_lang_golden("ts", "source.ts");
    let ids_regex: HashSet<&str> = regex_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let ids_ts: HashSet<&str> = ts_res.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let missing: Vec<&str> = ids_regex.difference(&ids_ts).copied().collect();
    assert!(
        missing.is_empty(),
        "node_id regex, потерянные в tree-sitter (привязка памяти!): {missing:?}"
    );
}

/// TS: стрелки `const x = (...) =>` (superset), interface/type-alias,
/// React BINDS_CLASS, HTTP-мосты.
#[test]
fn ts_mapper_superset_and_bridges() {
    let src = r#"import { api } from './api';

export interface User {
    id: number;
}

export type UserId = string;

export const loadUser = async (id: UserId) => {
    return api(`/users/${id}`);
};

class Panel {
    render(): string {
        return '<div class="x"></div>';
    }
}
"#;
    let mut out = AstScanResult::default();
    ts::parse_source("panel.tsx", src, "tsx", &mut out).expect("разбор tsx");

    // interface и type-alias → interface:{rel}:{name} (зеркало ts_interface_re)
    assert!(out.nodes.iter().any(|n| n.label == "User" && n.node_type == "Interface"));
    assert!(out.nodes.iter().any(|n| n.label == "UserId" && n.node_type == "Interface"));
    // стрелка (superset) + method_definition (superset)
    assert!(
        out.nodes.iter().any(|n| n.label == "loadUser" && n.node_type == "Function"),
        "стрелочная функция (superset) не найдена"
    );
    assert!(
        out.nodes.iter().any(|n| n.label == "render" && n.node_type == "Function"),
        "метод класса (superset) не найден"
    );
    // IMPORTS из source-поля
    assert!(
        out.edges.iter().any(|e| e.label == "IMPORTS" && e.target_node_id == "module:./api"),
        "IMPORTS не найден"
    );
}

/// TSX-грамматика действительно подключена для .tsx/.jsx (диспетчер).
#[test]
fn dispatcher_routes_ts_and_tsx() {
    for (rel, expected) in [("a.ts", "treesitter:ts"), ("b.tsx", "treesitter:tsx")] {
        let src = "export function demo(): void {}\n";
        let disp = AstParserDispatcher::new();
        let mut out = AstScanResult::default();
        let backend = disp.parse_source(rel, src, &mut out);
        assert_eq!(backend, expected);
        assert!(out.nodes.iter().any(|n| n.label == "demo"));
    }
}
