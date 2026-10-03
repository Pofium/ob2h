//! Тесты реестра языков/форматов AST-сканера (Ф47.5, PLAN_v1.7).
//! Новый формат (Ф48–Ф52) = запись в `LANGS` + фикстура в реестре +
//! минимальный тест ниже; инвариант консистентности проверяет всё автоматически.

use std::path::Path;

use ob2h::project::ast::{AstCodeExtractor, AstScanResult, LangSpec, LANGS};
use ob2h::project::watcher::is_code_file;

/// Хелпер: узел с указанной меткой есть в результате.
fn assert_has_node(res: &AstScanResult, label: &str) {
    assert!(
        res.nodes.iter().any(|n| n.label == label),
        "узел `{label}` не найден; есть: {:?}",
        res.nodes
            .iter()
            .map(|n| n.label.clone())
            .collect::<Vec<_>>()
    );
}

/// Хелпер: ребро с указанной меткой есть в результате.
fn assert_has_edge(res: &AstScanResult, label: &str) {
    assert!(
        res.edges.iter().any(|e| e.label == label),
        "ребро `{label}` не найдено; есть: {:?}",
        res.edges
            .iter()
            .map(|e| e.label.clone())
            .collect::<Vec<_>>()
    );
}

fn spec(name: &str) -> &'static LangSpec {
    LANGS
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("спецификация `{name}` отсутствует в реестре LANGS"))
}

fn parse(name: &str, rel_path: &str) -> AstScanResult {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    ex.parse_file(rel_path, spec(name).fixture, &mut res);
    res
}

/// Инвариант консистентности реестра (Ф47.5): для каждого `LangSpec` parse_file
/// на минимальной фикстуре не паникует, даёт ≥1 узел `File` и создаёт узлы/рёбра
/// только из заявленных в реестре словарей; путь-образец матчится на свою же
/// спецификацию; фикстура видна вотчеру (единство обходчика и вотчера, Ф47.2).
#[test]
fn test_registry_consistency() {
    assert!(!LANGS.is_empty(), "реестр LANGS пуст");
    for s in LANGS {
        let rel_path = s
            .sample_rel_path()
            .unwrap_or_else(|| panic!("у спецификации `{}` нет пути-образца", s.name));

        let matched = LangSpec::for_path(&rel_path)
            .unwrap_or_else(|| panic!("for_path вернул None на пути-образце {rel_path}"));
        assert_eq!(
            matched.name, s.name,
            "путь-образец `{rel_path}` матчится на `{}`, а не на `{}`",
            matched.name, s.name
        );

        let mut res = AstScanResult::default();
        AstCodeExtractor::new().parse_file(&rel_path, s.fixture, &mut res);

        assert!(
            res.nodes.iter().any(|n| n.node_type == "File"),
            "{}: парсер не создал узел File",
            s.name
        );
        for n in &res.nodes {
            assert!(
                s.node_types.contains(&n.node_type.as_str()),
                "{}: узел типа `{}` вне словаря спецификации {:?}",
                s.name,
                n.node_type,
                s.node_types
            );
        }
        for e in &res.edges {
            assert!(
                s.edge_labels.contains(&e.label.as_str()),
                "{}: ребро `{}` вне словаря спецификации {:?}",
                s.name,
                e.label,
                s.edge_labels
            );
        }

        // Вотчер согласован с обходчиком: файл реестра триггерит рескан
        assert!(
            is_code_file(Path::new(&rel_path)),
            "{}: вотчер не видит файл `{}` из реестра",
            s.name,
            rel_path
        );
    }
}

/// for_path: расширения (в т.ч. Windows-разделители и регистр), не-исходники.
#[test]
fn test_for_path_matching() {
    assert_eq!(LangSpec::for_path("src/main.rs").unwrap().name, "rust");
    assert_eq!(LangSpec::for_path("a\\b\\mod.PY").unwrap().name, "python");
    assert_eq!(LangSpec::for_path("app.tsx").unwrap().name, "ts-js");
    assert_eq!(LangSpec::for_path("x.h").unwrap().name, "c-cpp");
    // не исходники
    assert!(LangSpec::for_path("README.md").is_none());
    assert!(LangSpec::for_path("Cargo.lock").is_none());
    assert!(LangSpec::for_path("data/store.db").is_none());
    assert!(LangSpec::for_path(".gitignore").is_none());
    // исключаемые каталоги не влияют на матчинг пути как таковой
    // (фильтрация каталогов — обязанность collect_files/вотчера)
    assert_eq!(
        LangSpec::for_path("vendor/pkg/lib.rs").unwrap().name,
        "rust"
    );
}

/// Единый SKIP_DIRS (Ф47.2) содержит согласованный набор каталогов.
#[test]
fn test_skip_dirs_content() {
    for d in [
        ".git", "target", "node_modules", "dist", "build", "venv", ".venv", "vendor", "data",
        "logs", "backups", "__pycache__",
    ] {
        assert!(
            ob2h::project::ast::SKIP_DIRS.contains(&d),
            "SKIP_DIRS не содержит `{d}`"
        );
    }
    assert!(
        !ob2h::project::ast::SKIP_DIRS.contains(&"src"),
        "SKIP_DIRS не должен исключать исходники"
    );
}

/// Вотчер: исключаемые каталоги отсекаются до матчинга расширения.
#[test]
fn test_watcher_skips_dirs() {
    assert!(!is_code_file(Path::new("target/debug/ob2h.rs")));
    assert!(!is_code_file(Path::new("node_modules/x/index.js")));
    assert!(!is_code_file(Path::new(".git/config")));
    assert!(!is_code_file(Path::new("logs/run.sh")));
    assert!(is_code_file(Path::new("src/lib.rs")));
}

// --- Минимальные семантические тесты по одному на спецификацию реестра ---

#[test]
fn test_spec_rust() {
    let res = parse("rust", "src/lib.rs");
    assert_has_node(&res, "Row");
    assert_has_node(&res, "Repo");
    assert_has_node(&res, "health");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
    assert_has_edge(&res, "ROUTE");
}

#[test]
fn test_spec_python() {
    let res = parse("python", "app/model.py");
    assert_has_node(&res, "User");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
    assert_has_edge(&res, "QUERIES_TABLE");
    assert_has_edge(&res, "ROUTE");
}

#[test]
fn test_spec_ts_js() {
    let res = parse("ts-js", "src/app.ts");
    assert_has_node(&res, "UserService");
    assert_has_node(&res, "Repo");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
}

#[test]
fn test_spec_go() {
    let res = parse("go", "store/row.go");
    assert_has_node(&res, "Row");
    assert_has_node(&res, "Get");
    assert_has_edge(&res, "DEFINES");
}

#[test]
fn test_spec_sql() {
    let res = parse("sql", "migrations/init.sql");
    assert_has_node(&res, "users");
    assert_has_node(&res, "orders");
    assert_has_edge(&res, "FOREIGN_KEY_TO");
}

#[test]
fn test_spec_php() {
    let res = parse("php", "app/Models/UserRepository.php");
    assert_has_node(&res, "UserRepository");
    assert_has_node(&res, "Timestamps");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
    assert_has_edge(&res, "IMPLEMENTS");
}

#[test]
fn test_spec_dart() {
    let res = parse("dart", "lib/services/user.dart");
    assert_has_node(&res, "UserService");
    assert_has_node(&res, "Mix");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
    assert_has_edge(&res, "IMPLEMENTS");
}

#[test]
fn test_spec_java() {
    let res = parse("java", "src/main/java/UserService.java");
    assert_has_node(&res, "UserService");
    assert_has_node(&res, "Repo");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "IMPLEMENTS");
}

#[test]
fn test_spec_csharp() {
    let res = parse("csharp", "Services/UserService.cs");
    assert_has_node(&res, "UserService");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
}

#[test]
fn test_spec_c_cpp() {
    let res = parse("c-cpp", "src/row.c");
    assert_has_node(&res, "Row");
    assert_has_node(&res, "get");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
}

#[test]
fn test_spec_kotlin() {
    let res = parse("kotlin", "src/UserService.kt");
    assert_has_node(&res, "UserService");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
}

#[test]
fn test_spec_swift() {
    let res = parse("swift", "Sources/App/UserService.swift");
    assert_has_node(&res, "UserService");
    assert_has_node(&res, "Repo");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
}

#[test]
fn test_spec_ruby() {
    let res = parse("ruby", "app/models/row.rb");
    assert_has_node(&res, "Row");
    assert_has_node(&res, "get");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
}

#[test]
fn test_spec_sfc() {
    let res = parse("sfc", "src/components/Card.vue");
    // Ф47: SFC разбирает только <script>; <template>/<style> — Ф48
    assert_has_edge(&res, "IMPORTS");
    let node_types: Vec<&str> = res.nodes.iter().map(|n| n.node_type.as_str()).collect();
    assert!(node_types.contains(&"File"));
}

#[test]
fn test_spec_scala() {
    let res = parse("scala", "src/main/scala/UserService.scala");
    assert_has_node(&res, "UserService");
    assert_has_node(&res, "Repo");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "INHERITS");
    assert_has_edge(&res, "IMPLEMENTS");
}

#[test]
fn test_spec_lua() {
    let res = parse("lua", "scripts/store.lua");
    assert_has_node(&res, "get");
    assert_has_node(&res, "put");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
}

#[test]
fn test_spec_elixir() {
    let res = parse("elixir", "lib/store.ex");
    assert_has_node(&res, "Store");
    assert_has_node(&res, "Repo");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
}

#[test]
fn test_spec_shell() {
    let res = parse("shell", "scripts/deploy.sh");
    assert_has_node(&res, "get");
    assert_has_node(&res, "put");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
}

/// NODE_TYPE_BY_PREFIX (Ф47.3, ADR-AST-6): известные префиксы → тип и метка
/// без префикса; неизвестный → None (в scan_project даёт ExternalModule).
#[test]
fn test_node_type_for_prefix() {
    let (t, label) = ob2h::project::ast::node_type_for_prefix("route:GET /users").unwrap();
    assert_eq!(t, "Route");
    assert_eq!(label, "GET /users");

    let (t, label) = ob2h::project::ast::node_type_for_prefix("table:users").unwrap();
    assert_eq!(t, "Table");
    assert_eq!(label, "users");

    let (t, label) = ob2h::project::ast::node_type_for_prefix("pkg:rust:serde_json").unwrap();
    assert_eq!(t, "ExternalPackage");
    assert_eq!(label, "rust:serde_json");

    assert!(ob2h::project::ast::node_type_for_prefix("module:std").is_none());
}
