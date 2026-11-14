//! Ф70.10 (PLAN_v2.0): fuzz-гарантия отсутствия паник маппера CST→AST на
//! синтаксически некорректном коде. Диспетчер обязан вернуть результат (или
//! fallback на regex), никогда не паниковать. Запуск:
//! `cargo test --features treesitter` (порог 0.10 — fallback 70.9).
#![cfg(feature = "treesitter")]

use proptest::prelude::*;

use ob2h::project::ast_backend::AstParserDispatcher;

/// Код-подобный мусор: скобки, кавычки, идентификаторы, переводы строк.
/// Любые комбинации — вплоть до синтаксически бессмысленных.
fn garbage_strategy() -> impl proptest::strategy::Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9_{}()\"';:.&<>/+= \n\t-]{0,240}").unwrap()
}

// Диспетчер на всех 5 языках не паникует на мусоре (fallback 70.9 валиден).
proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(256))]

    #[test]
    fn dispatcher_never_panics_on_garbage(src in garbage_strategy()) {
        let disp = AstParserDispatcher::new();
        for rel in ["a.rs", "a.py", "a.ts", "a.go", "a.dart"] {
            let mut out = ob2h::project::ast::AstScanResult::default();
            let _ = disp.parse_source(rel, &src, &mut out); // Err/Ok — оба валидны
        }
    }
}

// Прямой вызов ts::parse_source (без fallback-обёртки) не паникует.
proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(128))]

    #[test]
    fn ts_parse_source_never_panics(src in garbage_strategy()) {
        for (rel, lang) in [
            ("a.rs", "rust"),
            ("a.py", "python"),
            ("a.ts", "ts"),
            ("a.go", "go"),
            ("a.dart", "dart"),
        ] {
            let mut out = ob2h::project::ast::AstScanResult::default();
            let _ = ob2h::project::ast_backend::ts::parse_source(rel, &src, lang, &mut out);
        }
    }
}

/// Политика 70.9: чистый код — tree-sitter; сильно битый — fallback на regex.
#[test]
fn fallback_policy_endpoints() {
    let clean = "pub fn ok() -> u64 { 42 }\n\npub struct S { pub a: u64 }\n";
    let disp = AstParserDispatcher::new();
    let mut out = ob2h::project::ast::AstScanResult::default();
    let backend = disp.parse_source("a.rs", clean, &mut out);
    assert_eq!(backend, "treesitter:rust", "чистый код должен идти в tree-sitter");

    // Сплошной мусор с ERROR-узлами → > 10% → fallback на regex
    // (диагностика доли — в Err-сообщении ts::parse_source). Проглоченный
    // lexer'ом мусор (`@`, `}`) даёт пустое дерево — безвреден, не ERROR.
    for garbage in [
        "let s = \"unclosed;\n@@@\n@@@\n", // 2/16 ERROR
        "fn f( { ) }\nfn g( { ) }\n",     // 3/18
        "use ;\nimpl { ; }\n",            // 2/13
    ] {
        let mut disp_out = ob2h::project::ast::AstScanResult::default();
        let backend = disp.parse_source("a.rs", garbage, &mut disp_out);
        assert_eq!(backend, "regex", "мусор {garbage:?} должен уходить в fallback");
    }
}
