//! Ф78b (issue #3): tree-sitter-волна для GAS-ассемблера — паритет node_id с
//! regex-путём, гейт диалекта (NASM/MASM → regex), kill-switch `OB2H_TREESITTER=0`,
//! F1-гейты против asm-gas эталона (70.11/71.7).
//! Запуск: `cargo test --features treesitter`.
#![cfg(feature = "treesitter")]

use ob2h::project::ast::{resolve_calls, AstCodeExtractor, AstScanResult};
use ob2h::project::ast_backend::AstParserDispatcher;
use ob2h::project::ast_golden::{f1, load_all, CALL_F1_MIN, SYMBOL_F1_MIN};

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ast_golden")
}

fn regex_parse(rel_path: &str, src: &str) -> AstScanResult {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    ex.parse_file(rel_path, src, &mut res);
    res
}

fn dispatcher_parse(rel_path: &str, src: &str) -> (AstScanResult, String) {
    let disp = AstParserDispatcher::new();
    let mut res = AstScanResult::default();
    let backend = disp.parse_source(rel_path, src, &mut res);
    (res, backend)
}

/// Env-мьютекс: диспетчер читает `OB2H_TREESITTER` в new() — параллельные
/// тесты этого бинарника не должны видеть чужой kill-switch.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Ф78b.3: паритет node_id/рёбер/call_sites между regex-путём и ts::map_asm
/// на asm-gas эталоне (стабильность привязки памяти и Ralph-findings).
#[test]
fn test_asm_ts_node_id_parity_with_regex() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let fixture = fixtures_dir().join("asm-gas/source.s");
    let src = std::fs::read_to_string(&fixture).expect("fixture");
    let (ts_res, backend) = dispatcher_parse("source.s", &src);
    assert_eq!(backend, "treesitter:asm", "GAS → tree-sitter бэкенд");

    let rx_res = regex_parse("source.s", &src);
    let ids = |r: &AstScanResult| -> Vec<String> {
        let mut v: Vec<String> = r.nodes.iter().map(|n| n.node_id.clone()).collect();
        v.sort();
        v
    };
    assert_eq!(ids(&ts_res), ids(&rx_res), "node_id паритет");

    let edges = |r: &AstScanResult| -> Vec<String> {
        let mut v: Vec<String> = r
            .edges
            .iter()
            .map(|e| format!("{}|{}|{}", e.source_node_id, e.target_node_id, e.label))
            .collect();
        v.sort();
        v
    };
    assert_eq!(edges(&ts_res), edges(&rx_res), "паритет рёбер");

    let calls = |r: &AstScanResult| -> Vec<String> {
        let mut v: Vec<String> = r
            .call_sites
            .iter()
            .map(|c| format!("{}|{}", c.line, c.name))
            .collect();
        v.sort();
        v
    };
    assert_eq!(calls(&ts_res), calls(&rx_res), "паритет call_sites");
}

/// Ф78b.3: NASM/MASM-контент остаётся на regex (generic-грамматик нет).
#[test]
fn test_asm_nasm_masm_content_uses_regex_backend() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let nasm = "%include \"io.asm\"\nsection .text\nglobal start\nstart:\n    call work\n    ret\n";
    let (_, backend) = dispatcher_parse("n.asm", nasm);
    assert_eq!(backend, "regex", "NASM-контент → regex");

    let masm = ".MODEL SMALL\n.CODE\nP PROC\n    ret\nP ENDP\nEND\n";
    let (_, backend) = dispatcher_parse("m.asm", masm);
    assert_eq!(backend, "regex", "MASM-контент → regex");
}

/// Ф78b.4: kill-switch `OB2H_TREESITTER=0` возвращает regex для asm-файлов.
#[test]
fn test_asm_ts_kill_switch() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let src = std::fs::read_to_string(fixtures_dir().join("asm-gas/source.s")).expect("fixture");
    std::env::set_var("OB2H_TREESITTER", "0");
    let (_, backend) = dispatcher_parse("source.s", &src);
    std::env::remove_var("OB2H_TREESITTER");
    assert_eq!(backend, "regex", "kill-switch → regex для всех asm-файлов");
}

/// Ф78b.4: F1-гейты 70.11/71.7 против asm-gas эталона на CST-бэкенде.
#[test]
fn test_asm_ts_f1_gates_on_golden() {
    let _env = ENV_LOCK.lock().expect("env lock");
    let items = load_all(&fixtures_dir()).expect("golden");
    let (fixture_path, golden) = items
        .iter()
        .find(|(_, g)| g.language == "asm-gas")
        .expect("asm-gas эталон")
        .clone();
    let src = std::fs::read_to_string(&fixture_path).expect("fixture");
    let (res, backend) = dispatcher_parse(&golden.fixture, &src);
    assert_eq!(backend, "treesitter:asm");
    let mut res = res;
    resolve_calls(&mut res);

    // symbol F1: предсказание — Function/Macro/Data (словарь asm-реестра)
    let predicted: std::collections::HashSet<(String, String)> = res
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.node_type.as_str(), "Function" | "Macro" | "Data" | "Struct" | "Class")
        })
        .map(|n| (n.label.clone(), n.node_type.clone()))
        .collect();
    let required: std::collections::HashSet<(String, String)> = golden
        .required_symbols
        .iter()
        .map(|s| (s.name.clone(), s.kind.clone()))
        .collect();
    let tp = predicted.intersection(&required).count();
    let sym_f1 = f1(tp, predicted.len() - tp, required.len() - tp);
    assert!(
        sym_f1 >= SYMBOL_F1_MIN,
        "symbol F1 {sym_f1:.3} < {SYMBOL_F1_MIN} (predicted={predicted:?})"
    );

    // call F1: RESOLVED CALLS
    let pred_calls: std::collections::HashSet<(String, String)> = res
        .edges
        .iter()
        .filter(|e| e.label == "CALLS" && e.provenance != "HEURISTIC")
        .map(|e| {
            let label = |nid: &str| {
                res.nodes
                    .iter()
                    .find(|n| n.node_id == nid)
                    .map(|n| n.label.clone())
                    .unwrap_or_default()
            };
            (label(&e.source_node_id), label(&e.target_node_id))
        })
        .collect();
    let req_calls: std::collections::HashSet<(String, String)> = golden
        .required_calls
        .iter()
        .map(|c| (c.from.clone(), c.to.clone()))
        .collect();
    let ctp = pred_calls.intersection(&req_calls).count();
    let call_f1 = f1(ctp, pred_calls.len() - ctp, req_calls.len() - ctp);
    assert!(
        call_f1 >= CALL_F1_MIN,
        "call F1 {call_f1:.3} < {CALL_F1_MIN} (predicted={pred_calls:?})"
    );
}
