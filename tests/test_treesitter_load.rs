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
