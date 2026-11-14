//! Ф69.3 (PLAN_v2.0): валидация `ast_golden` — целостность эталонов,
//! гарантированный уровень текущего regex-парсера и детерминизм (parity hash,
//! NFR «Детерминизм AST»). Эталоны — источник для гейтов 70.11/71.7 при
//! переходе на tree-sitter (symbol F1 ≥ 0.95, call F1 ≥ 0.90).

use std::collections::HashSet;
use std::path::PathBuf;

use ob2h::graph::callpath::SYMBOL_TYPES;
use ob2h::project::ast::{resolve_calls, AstCodeExtractor, AstScanResult};
use ob2h::project::ast_golden::{
    f1, load_all, CALL_F1_MIN, SYMBOL_F1_MIN,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ast_golden")
}

fn parse(rel_path: &str, source: &str) -> AstScanResult {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    ex.parse_file(rel_path, source, &mut res);
    resolve_calls(&mut res);
    res
}

/// Целостность эталона: 5 языков, имена символов/вызовов реально встречаются
/// в фикстуре (защита от опечаток в expected.json).
#[test]
fn golden_fixtures_integrity() {
    let items = load_all(&fixtures_dir()).expect("загрузка ast_golden");
    assert_eq!(items.len(), 5, "эталоны для rust, python, ts, go, dart");
    let langs: Vec<&str> = items.iter().map(|(_, g)| g.language.as_str()).collect();
    for lang in ["rust", "python", "ts", "go", "dart"] {
        assert!(langs.contains(&lang), "нет эталона {lang}");
    }
    for (fixture_path, g) in &items {
        let src = std::fs::read_to_string(fixture_path).expect("фикстура читается");
        assert!(!g.notes.trim().is_empty(), "{}: пустые notes", g.language);
        for sym in &g.required_symbols {
            assert!(
                src.contains(&sym.name),
                "{}: символ `{}` отсутствует в фикстуре",
                g.language,
                sym.name
            );
        }
        for call in &g.required_calls {
            assert!(
                src.contains(&call.from) && src.contains(&call.to),
                "{}: вызов {} → {} ссылается на отсутствующие имена",
                g.language,
                call.from,
                call.to
            );
        }
    }
}

/// Гарантированный уровень текущего парсера: все required_symbols находятся
/// (имя + node_type); для `call_extraction: "current"` — все required_calls
/// резолвятся в рёбра CALLS (type-pass, Ф39.1).
#[test]
fn current_parser_meets_required_level() {
    let items = load_all(&fixtures_dir()).expect("загрузка ast_golden");
    for (fixture_path, g) in &items {
        let src = std::fs::read_to_string(fixture_path).expect("фикстура читается");
        let res = parse(&g.fixture, &src);

        for sym in &g.required_symbols {
            let found = res
                .nodes
                .iter()
                .any(|n| n.label == sym.name && n.node_type == sym.kind);
            assert!(
                found,
                "{}: символ `{}` ({}) не найден; узлы: {:?}",
                g.language,
                sym.name,
                sym.kind,
                res.nodes
                    .iter()
                    .map(|n| format!("{}:{}", n.node_type, n.label))
                    .collect::<Vec<_>>()
            );
        }

        if g.call_extraction == "current" {
            for call in &g.required_calls {
                let from = res
                    .nodes
                    .iter()
                    .find(|n| n.label == call.from)
                    .map(|n| n.node_id.clone());
                let to = res
                    .nodes
                    .iter()
                    .find(|n| n.label == call.to)
                    .map(|n| n.node_id.clone());
                assert!(
                    from.is_some() && to.is_some(),
                    "{}: концы вызова {} → {} не резолвятся в узлы",
                    g.language,
                    call.from,
                    call.to
                );
                let (from, to) = (from.unwrap(), to.unwrap());
                let edge_found = res.edges.iter().any(|e| {
                    e.source_node_id == from && e.target_node_id == to && e.label == "CALLS"
                });
                assert!(
                    edge_found,
                    "{}: ребро CALLS {} → {} отсутствует; рёбра: {:?}",
                    g.language,
                    call.from,
                    call.to,
                    res.edges
                        .iter()
                        .map(|e| format!("{}:{}:{}", e.label, e.source_node_id, e.target_node_id))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

/// Гейт 71.7: symbol F1 ≥ 0.95 на всех языках (regex-бэкенд — дефолтный путь
/// продакшена), call F1 ≥ 0.90 для языков с гарантированными вызовами (current).
#[test]
fn f1_gates_regex_backend() {
    let items = load_all(&fixtures_dir()).expect("загрузка ast_golden");
    for (fixture_path, g) in &items {
        let src = std::fs::read_to_string(fixture_path).expect("фикстура читается");
        let res = parse(&g.fixture, &src);

        let predicted: HashSet<(String, String)> = res
            .nodes
            .iter()
            .filter(|n| SYMBOL_TYPES.contains(&n.node_type.as_str()))
            .map(|n| (n.label.clone(), n.node_type.clone()))
            .collect();
        let required: HashSet<(String, String)> = g
            .required_symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind.clone()))
            .collect();
        let tp = predicted.intersection(&required).count();
        let sym_f1 = f1(tp, predicted.len() - tp, required.len() - tp);
        assert!(
            sym_f1 >= SYMBOL_F1_MIN,
            "{}: symbol F1 {:.3} < {:.2}; предсказано {:?}, требовалось {:?}",
            g.language,
            sym_f1,
            SYMBOL_F1_MIN,
            predicted,
            required
        );

        let label_of = |nid: &str| -> String {
            res.nodes
                .iter()
                .find(|n| n.node_id == nid)
                .map(|n| n.label.clone())
                .unwrap_or_default()
        };
        let pred_calls: HashSet<(String, String)> = res
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.provenance != "HEURISTIC")
            .map(|e| (label_of(&e.source_node_id), label_of(&e.target_node_id)))
            .collect();
        let req_calls: HashSet<(String, String)> = g
            .required_calls
            .iter()
            .map(|c| (c.from.clone(), c.to.clone()))
            .collect();
        let ctp = pred_calls.intersection(&req_calls).count();
        let call_f1 = f1(ctp, pred_calls.len() - ctp, req_calls.len() - ctp);
        if g.call_extraction == "current" {
            assert!(
                call_f1 >= CALL_F1_MIN,
                "{}: call F1 {:.3} < {:.2}; предсказано {:?}, требовалось {:?}",
                g.language,
                call_f1,
                CALL_F1_MIN,
                pred_calls,
                req_calls
            );
        }
    }
}

/// Гейт 71.7 под tree-sitter (Фаза 70 — ядро глубокого call graph): все 5
/// языков дают и symbol F1 ≥ 0.95, и call F1 ≥ 0.90.
#[cfg(feature = "treesitter")]
#[test]
fn f1_gates_treesitter_backend() {
    use ob2h::project::ast_backend::ts;

    let items = load_all(&fixtures_dir()).expect("загрузка ast_golden");
    for (fixture_path, g) in &items {
        let src = std::fs::read_to_string(fixture_path).expect("фикстура читается");
        let lang = match g.language.as_str() {
            "rust" => "rust",
            "python" => "python",
            "ts" => "ts",
            "go" => "go",
            "dart" => "dart",
            other => panic!("неизвестный язык {other}"),
        };
        let mut res = AstScanResult::default();
        ts::parse_source(&g.fixture, &src, lang, &mut res).expect("tree-sitter разбор");
        resolve_calls(&mut res);

        let predicted: HashSet<(String, String)> = res
            .nodes
            .iter()
            .filter(|n| SYMBOL_TYPES.contains(&n.node_type.as_str()))
            .map(|n| (n.label.clone(), n.node_type.clone()))
            .collect();
        let required: HashSet<(String, String)> = g
            .required_symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind.clone()))
            .collect();
        let tp = predicted.intersection(&required).count();
        let sym_f1 = f1(tp, predicted.len() - tp, required.len() - tp);
        assert!(sym_f1 >= SYMBOL_F1_MIN, "{}: symbol F1 {:.3}", g.language, sym_f1);

        let label_of = |nid: &str| -> String {
            res.nodes
                .iter()
                .find(|n| n.node_id == nid)
                .map(|n| n.label.clone())
                .unwrap_or_default()
        };
        let pred_calls: HashSet<(String, String)> = res
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.provenance != "HEURISTIC")
            .map(|e| (label_of(&e.source_node_id), label_of(&e.target_node_id)))
            .collect();
        let req_calls: HashSet<(String, String)> = g
            .required_calls
            .iter()
            .map(|c| (c.from.clone(), c.to.clone()))
            .collect();
        let ctp = pred_calls.intersection(&req_calls).count();
        let call_f1 = f1(ctp, pred_calls.len() - ctp, req_calls.len() - ctp);
        assert!(
            call_f1 >= CALL_F1_MIN,
            "{}: call F1 {:.3} < {:.2}; предсказано {:?}, требовалось {:?}",
            g.language,
            call_f1,
            CALL_F1_MIN,
            pred_calls,
            req_calls
        );
    }
}

/// Детерминизм (NFR): идентичный вход → идентичные node_id и множество рёбер.
#[test]
fn parity_hash_determinism() {
    let items = load_all(&fixtures_dir()).expect("загрузка ast_golden");
    for (fixture_path, g) in &items {
        let src = std::fs::read_to_string(fixture_path).expect("фикстура читается");
        let a = parse(&g.fixture, &src);
        let b = parse(&g.fixture, &src);

        let nodes_a: HashSet<&str> = a.nodes.iter().map(|n| n.node_id.as_str()).collect();
        let nodes_b: HashSet<&str> = b.nodes.iter().map(|n| n.node_id.as_str()).collect();
        assert_eq!(nodes_a, nodes_b, "{}: node_id разошлись", g.language);

        let edges_key = |r: &AstScanResult| -> HashSet<String> {
            r.edges
                .iter()
                .map(|e| {
                    format!(
                        "{}|{}|{}",
                        e.source_node_id, e.target_node_id, e.label
                    )
                })
                .collect()
        };
        assert_eq!(
            edges_key(&a),
            edges_key(&b),
            "{}: множество рёбер разошлось",
            g.language
        );
    }
}
