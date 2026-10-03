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
    // script → импорты; template → BINDS_CLASS и компоненты; style → CssClass
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "BINDS_CLASS");
    assert_has_edge(&res, "DEFINES");
    // класс .card определён и в <style>, и связан в <template> — узел один
    let class_nodes: Vec<_> = res
        .nodes
        .iter()
        .filter(|n| n.node_type == "CssClass" && n.label == "card")
        .collect();
    assert_eq!(class_nodes.len(), 1, "класс .card должен быть одним узлом");
}

#[test]
fn test_spec_html() {
    let res = parse("html", "public/index.html");
    // Element по id
    assert_has_node(&res, "div#header");
    assert_has_node(&res, "input#login");
    let el = res.nodes.iter().find(|n| n.label == "div#header").unwrap();
    assert_eq!(el.node_type, "Element");
    assert_eq!(el.file_path, "public/index.html");
    // стили/скрипты/классы/маршруты
    assert_has_edge(&res, "STYLES");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "BINDS_CLASS");
    assert_has_edge(&res, "ROUTE");
}

#[test]
fn test_spec_html_routes() {
    let res = parse("html", "public/index.html");
    let route_targets: Vec<&str> = res
        .edges
        .iter()
        .filter(|e| e.label == "ROUTE")
        .map(|e| e.target_node_id.as_str())
        .collect();
    assert!(
        route_targets.contains(&"route:GET /users"),
        "href → GET: {:?}",
        route_targets
    );
    assert!(
        route_targets.contains(&"route:POST /api/login"),
        "form method=post"
    );
    assert!(route_targets.contains(&"route:POST /api/save"), "hx-post");
    // внешний URL и якоря не дают маршрутов (в фикстуре их нет — проверяем
    // отсутствие мусорных route:-целей без ведущего "/")
    for t in &route_targets {
        let Some(path) = t.split_once(' ').map(|(_, p)| p) else {
            panic!("маршрут без метода: {t}");
        };
        assert!(path.starts_with('/'), "маршрут без in-app пути: {t}");
    }
}

#[test]
fn test_spec_blade() {
    let res = parse("blade", "resources/views/home.blade.php");
    // blade идёт HTML-парсером (Ф48.1), а не PHP-экстрактором
    assert_has_node(&res, "div#page");
    assert_has_edge(&res, "BINDS_CLASS");
    assert_has_edge(&res, "ROUTE");
    assert_eq!(
        LangSpec::for_path("resources/views/home.blade.php")
            .unwrap()
            .name,
        "blade"
    );
    // при этом обычный .php — по-прежнему PHP
    assert_eq!(
        LangSpec::for_path("app/Models/User.php").unwrap().name,
        "php"
    );
}

#[test]
fn test_spec_css() {
    let res = parse("css", "assets/app.css");
    assert_has_node(&res, "card");
    assert_has_node(&res, "sidebar");
    assert_has_node(&res, "main-color");
    let cls = res.nodes.iter().find(|n| n.label == "card").unwrap();
    assert_eq!(cls.node_type, "CssClass");
    let var = res.nodes.iter().find(|n| n.label == "main-color").unwrap();
    assert_eq!(var.node_type, "CssVar");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
    // 48.5: дедуп — .card встречается в фикстуре дважды (корень и @media) →
    // один узел; номер строки — первого вхождения
    let card_count = res
        .nodes
        .iter()
        .filter(|n| n.node_type == "CssClass" && n.label == "card")
        .count();
    assert_eq!(card_count, 1);
}

/// 48.5: 200 правил с `.card` → один узел `cssclass:card`.
#[test]
fn test_css_class_dedup_many_rules() {
    let mut css = String::new();
    for i in 0..200 {
        css.push_str(&format!(".rule{} {{ color: red; }}\n", i));
        css.push_str(".card { margin: 0; }\n");
    }
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    ex.parse_file("assets/big.css", &css, &mut res);
    let card_count = res
        .nodes
        .iter()
        .filter(|n| n.node_type == "CssClass" && n.label == "card")
        .count();
    assert_eq!(card_count, 1, "200 повторов .card должны дать один узел");
    assert_eq!(
        res.nodes
            .iter()
            .filter(|n| n.node_type == "CssClass")
            .count(),
        201
    );
}

/// Цели рёбер Ф48: file:/css: узлы типизируются таблицей префиксов (ADR-AST-6).
#[test]
fn test_node_type_for_prefix_48() {
    let (t, label) = ob2h::project::ast::node_type_for_prefix("file:assets/app.css").unwrap();
    assert_eq!(t, "File");
    assert_eq!(label, "assets/app.css");
    let (t, label) = ob2h::project::ast::node_type_for_prefix("css:assets/tailwind").unwrap();
    assert_eq!(t, "Stylesheet");
    assert_eq!(label, "assets/tailwind");
    let (t, label) = ob2h::project::ast::node_type_for_prefix("cssclass:card").unwrap();
    assert_eq!(t, "CssClass");
    assert_eq!(label, "card");
}

#[test]
fn test_spec_jupyter() {
    let res = parse("jupyter", "notebooks/analysis.ipynb");
    // Notebook-узел + символы из code-ячеек; markdown-ячейка игнорируется
    assert_has_node(&res, "analysis.ipynb");
    assert_has_node(&res, "load");
    assert_has_node(&res, "Repo");
    assert_has_node(&res, "get");
    let nb = res
        .nodes
        .iter()
        .find(|n| n.node_type == "Notebook")
        .expect("нет Notebook-узла");
    assert!(nb.description.contains("code-ячеек: 2"));
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
    assert_has_edge(&res, "INHERITS");
    // 52.1: outputs не читаются — заглушка base64 не породила узлов
    assert!(
        !res.nodes.iter().any(|n| n.label.contains("base64")),
        "outputs не должны парситься"
    );
    // номера строк — по склеенному коду ячеек (offset сквозной)
    let load = res.nodes.iter().find(|n| n.label == "load").unwrap();
    assert_eq!(load.line_start, 2, "первая строка первой code-ячейки");
    let get = res.nodes.iter().find(|n| n.label == "get").unwrap();
    assert!(get.line_start > load.line_start, "offset второй ячейки больше");
}

/// «ipynb без cells» — только File-узел (52.1), без паники.
#[test]
fn test_notebook_without_cells() {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    ex.parse_file(
        "broken.ipynb",
        r#"{"metadata": {}, "nbformat": 4}"#,
        &mut res,
    );
    assert!(res.nodes.iter().all(|n| n.node_type == "File"));
}

#[test]
fn test_spec_solidity() {
    let res = parse("solidity", "contracts/Token.sol");
    assert_has_node(&res, "Token");
    assert_has_node(&res, "transfer");
    assert_has_node(&res, "balances");
    let mapping = res
        .nodes
        .iter()
        .find(|n| n.node_type == "Mapping")
        .expect("нет Mapping-узла");
    assert_eq!(mapping.label, "balances");
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "DEFINES");
    // `is IToken, Ownable`: первая база INHERITS, вторая IMPLEMENTS
    let inherits: Vec<_> = res
        .edges
        .iter()
        .filter(|e| e.label == "INHERITS")
        .map(|e| e.target_node_id.clone())
        .collect();
    assert!(inherits.contains(&"class:IToken".to_string()), "{:?}", inherits);
    let implements: Vec<_> = res
        .edges
        .iter()
        .filter(|e| e.label == "IMPLEMENTS")
        .map(|e| e.target_node_id.clone())
        .collect();
    assert!(implements.contains(&"interface:Ownable".to_string()), "{:?}", implements);
    assert_has_edge(&res, "USES_SCHEMA");
    // event/constructor тоже Function-узлы
    assert_has_node(&res, "Transfer");
    assert!(res
        .nodes
        .iter()
        .any(|n| n.label == "constructor" && n.node_type == "Function"));
}

#[test]
fn test_spec_zig() {
    let res = parse("zig", "src/main.zig");
    assert_has_node(&res, "main");
    assert_has_node(&res, "compute");
    assert_has_node(&res, "Point");
    let point = res.nodes.iter().find(|n| n.label == "Point").unwrap();
    assert_eq!(point.node_type, "Struct");
    // pub const (не структура) → Variable
    let version = res.nodes.iter().find(|n| n.label == "version").unwrap();
    assert_eq!(version.node_type, "Variable");
    // @import("std") → pkg:zig:std, @import("mod.zig") → file:
    let imports: Vec<_> = res
        .edges
        .iter()
        .filter(|e| e.label == "IMPORTS")
        .map(|e| e.target_node_id.clone())
        .collect();
    assert!(imports.contains(&"pkg:zig:std".to_string()), "{:?}", imports);
    assert!(imports.contains(&"file:mod.zig".to_string()), "{:?}", imports);
    // тест → Function с label test_* (совместимо с entrypoint_reason)
    let test = res
        .nodes
        .iter()
        .find(|n| n.label == "test_compute doubles")
        .expect("zig-тест не найден");
    assert_eq!(test.node_type, "Function");
    assert_has_edge(&res, "DEFINES");
}

// --- Ф49: API-контракты (proto / graphql / openapi) и мосты ---

#[test]
fn test_spec_proto() {
    let res = parse("proto", "proto/demo.proto");
    assert_has_node(&res, "demo.v1");
    assert_has_node(&res, "UserService");
    assert_has_node(&res, "GetUser");
    assert_has_node(&res, "User");
    assert_has_node(&res, "Status");
    let svc = res.nodes.iter().find(|n| n.node_type == "Service").unwrap();
    assert!(svc.node_id.starts_with("service:"));
    // rpc внутри сервиса: service →(DEFINES)→ rpc
    assert!(res.edges.iter().any(|e| e.label == "DEFINES"
        && e.source_node_id.ends_with("service:proto/demo.proto:UserService")
        && e.target_node_id.ends_with("fn:proto/demo.proto:GetUser")));
    assert_has_edge(&res, "IMPORTS");
    assert_has_edge(&res, "USES_SCHEMA");
}

#[test]
fn test_spec_graphql() {
    let res = parse("graphql", "schema/schema.graphql");
    assert_has_node(&res, "User");
    assert_has_node(&res, "Node");
    assert_has_node(&res, "Role");
    // поля Query/Mutation → Function-узлы
    assert_has_node(&res, "users");
    assert_has_node(&res, "createUser");
    let users = res.nodes.iter().find(|n| n.label == "users").unwrap();
    assert_eq!(users.node_type, "Function");
    assert!(users.description.contains("Query"));
    let cu = res.nodes.iter().find(|n| n.label == "createUser").unwrap();
    assert!(cu.description.contains("Mutation"));
    assert_has_edge(&res, "DEFINES");
}

/// 49.5: openapi.json и openapi.yaml дают одинаковый семантический результат.
#[test]
fn test_openapi_json_yaml_parity() {
    let ex = AstCodeExtractor::new();
    let mut jres = AstScanResult::default();
    ex.parse_file("openapi.json", spec("openapi").fixture, &mut jres);

    let yaml = r##"openapi: 3.0.0
info:
  title: Demo API
  version: 1.0.0
paths:
  /api/users:
    get:
      summary: Список пользователей
      parameters:
        - name: limit
          in: query
          schema:
            type: integer
    post:
      requestBody:
        $ref: "#/components/schemas/NewUser"
components:
  schemas:
    NewUser:
      type: object
      properties:
        profile:
          $ref: "#/components/schemas/Profile"
    Profile:
      type: object
"##;
    let mut yres = AstScanResult::default();
    ex.parse_file("openapi.yaml", yaml, &mut yres);

    // node_id содержит имя файла — нормализуем для сравнения семантики
    let norm = |s: &str| s.replace("openapi.json", "OPENAPI").replace("openapi.yaml", "OPENAPI");
    let sig = |r: &AstScanResult| {
        let mut nodes: Vec<(String, String, String)> = r
            .nodes
            .iter()
            .filter(|n| n.node_type != "File")
            .map(|n| (norm(&n.node_id), n.node_type.clone(), n.label.clone()))
            .collect();
        nodes.sort();
        let mut edges: Vec<(String, String, String)> = r
            .edges
            .iter()
            .map(|e| {
                (
                    norm(&e.source_node_id),
                    norm(&e.target_node_id),
                    e.label.clone(),
                )
            })
            .collect();
        edges.sort();
        (nodes, edges)
    };
    assert_eq!(sig(&jres), sig(&yres), "json и yaml должны быть эквивалентны");

    // ключевые узлы: endpoint + парный route: (тот же id, что у серверных роутов)
    assert!(jres.nodes.iter().any(|n| n.node_id == "route:GET /api/users"));
    assert!(jres
        .nodes
        .iter()
        .any(|n| n.node_type == "Endpoint" && n.label == "GET /api/users"));
    assert!(jres.nodes.iter().any(|n| n.node_type == "Struct" && n.label == "NewUser"));
    // route: →(SERVES_ENDPOINT)→ endpoint
    assert!(jres.edges.iter().any(|e| e.label == "SERVES_ENDPOINT"
        && e.source_node_id == "route:GET /api/users"));
    // requestBody $ref → USES_SCHEMA к schema-узлу
    assert!(jres.edges.iter().any(|e| e.label == "USES_SCHEMA"
        && e.target_node_id == "struct:openapi.json:NewUser"));
    // parameters → Field
    assert!(jres.nodes.iter().any(|n| n.node_type == "Field" && n.label == "limit"));
}

/// 49.4: мосты — fetch('/api/users') → SERVES_ENDPOINT к route:-узлу;
/// похожий, но другой путь связи не даёт (негативный тест на шум).
#[test]
fn test_http_bridges() {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    // контракт
    ex.parse_file("openapi.json", spec("openapi").fixture, &mut res);
    // клиент с точным и неточным путём
    ex.parse_file(
        "src/client.ts",
        r#"import { api } from './api';

export function load() {
    return fetch("/api/users");
}

export function loadWrong() {
    return axios.get("/api/user");
}
"#,
        &mut res,
    );
    ob2h::project::ast::resolve_bridges(&mut res);

    let serves: Vec<_> = res
        .edges
        .iter()
        .filter(|e| e.label == "SERVES_ENDPOINT" && e.source_node_id == "file:src/client.ts")
        .map(|e| e.target_node_id.clone())
        .collect();
    assert!(
        serves.contains(&"route:GET /api/users".to_string()),
        "точное совпадение даёт мост: {:?}",
        serves
    );
    assert!(
        !serves.iter().any(|t| t.contains("/api/user ")),
        "похожий путь /api/user не должен матчиться: {:?}",
        serves
    );
}

/// 49.4г: proto rpc ↔ хендлер по нормализованному имени (camelCase↔snake_case).
#[test]
fn test_rpc_bridge() {
    let ex = AstCodeExtractor::new();
    let mut res = AstScanResult::default();
    ex.parse_file("proto/demo.proto", spec("proto").fixture, &mut res);
    ex.parse_file(
        "src/handler.rs",
        r#"pub fn get_user(id: u64) -> Option<User> {
    None
}
"#,
        &mut res,
    );
    ob2h::project::ast::resolve_bridges(&mut res);
    assert!(res.edges.iter().any(|e| e.label == "PROVIDES_RPC"
        && e.source_node_id == "fn:src/handler.rs:get_user"
        && e.target_node_id == "fn:proto/demo.proto:GetUser"));
}

// --- Ф50: IaC (Terraform / Dockerfile / compose / k8s) ---

#[test]
fn test_spec_terraform() {
    let mut res = parse("terraform", "infra/main.tf");
    ob2h::project::ast::resolve_bridges(&mut res);
    assert_has_node(&res, "yandex_compute_instance.app");
    let r = res
        .nodes
        .iter()
        .find(|n| n.label == "yandex_compute_instance.app")
        .unwrap();
    assert_eq!(r.node_type, "Resource");
    assert!(r.description.contains("провайдер: yandex"));
    assert_has_node(&res, "network");
    assert_has_node(&res, "region");
    assert_has_node(&res, "endpoint");
    // module source ./modules/vpc → file:
    assert!(res.edges.iter().any(|e| e.label == "DEPENDS_ON"
        && e.source_node_id.ends_with("module:infra/main.tf:network")
        && e.target_node_id == "file:modules/vpc"));
    // depends_on → ресурс из этого же скана (PendingRef резолв)
    assert!(res.edges.iter().any(|e| e.label == "DEPENDS_ON"
        && e.target_node_id.ends_with("resource:infra/main.tf:yandex_compute_instance.app")));
}

#[test]
fn test_spec_dockerfile() {
    let res = parse("dockerfile", "Dockerfile");
    // два FROM (multi-stage) → два BaseImage, без узлов стейджей
    let images: Vec<_> = res
        .nodes
        .iter()
        .filter(|n| n.node_type == "BaseImage")
        .map(|n| n.label.as_str())
        .collect();
    assert!(images.contains(&"node:20-alpine"), "{:?}", images);
    assert!(images.contains(&"nginx:1.27"), "{:?}", images);
    assert_eq!(images.len(), 2);
    // ARG/ENV → EnvVar
    let envs: Vec<_> = res
        .nodes
        .iter()
        .filter(|n| n.node_type == "EnvVar")
        .map(|n| n.label.as_str())
        .collect();
    assert!(envs.contains(&"NODE_ENV"), "{:?}", envs);
    assert!(envs.contains(&"APP_HOME"), "{:?}", envs);
    // COPY локальных путей → DEPENDS_ON к file:
    assert!(res.edges.iter().any(|e| e.label == "DEPENDS_ON"
        && e.target_node_id == "file:package.json"));
    // WORKDIR/EXPOSE/CMD — в description файла
    let file = res.nodes.iter().find(|n| n.node_type == "File").unwrap();
    assert!(file.description.contains("EXPOSE 3000"));
    assert!(file.description.contains("CMD"));
}

#[test]
fn test_spec_compose() {
    let res = parse("compose", "compose.yaml");
    assert_has_node(&res, "api");
    assert_has_node(&res, "db");
    let api = res.nodes.iter().find(|n| n.label == "api").unwrap();
    assert_eq!(api.node_type, "Service");
    // image → BaseImage
    assert!(res
        .nodes
        .iter()
        .any(|n| n.node_type == "BaseImage" && n.label == "postgres:16"));
    // build → file:api/Dockerfile
    assert!(res.edges.iter().any(|e| e.label == "DEPENDS_ON"
        && e.source_node_id.ends_with("svc:compose.yaml:api")
        && e.target_node_id == "file:api/Dockerfile"));
    // depends_on api → db
    assert!(res.edges.iter().any(|e| e.label == "DEPENDS_ON"
        && e.source_node_id.ends_with("svc:compose.yaml:api")
        && e.target_node_id.ends_with("svc:compose.yaml:db")));
    // ports → Port, volumes/networks → MOUNTS
    assert!(res.nodes.iter().any(|n| n.node_type == "Port" && n.label.contains("8080")));
    assert!(res.edges.iter().any(|e| e.label == "MOUNTS"
        && e.source_node_id.ends_with("svc:compose.yaml:api")));
    // environment → EnvVar
    assert!(res.nodes.iter().any(|n| n.node_type == "EnvVar" && n.label == "RUST_LOG"));
}

#[test]
fn test_spec_k8s() {
    let mut res = parse("k8s", "k8s/app.yaml");
    ob2h::project::ast::resolve_bridges(&mut res);
    assert_has_node(&res, "Deployment/api");
    assert_has_node(&res, "ConfigMap/api-config");
    let dep = res
        .nodes
        .iter()
        .find(|n| n.label == "Deployment/api")
        .unwrap();
    assert_eq!(dep.node_type, "K8sResource");
    // image → BaseImage
    assert!(res
        .nodes
        .iter()
        .any(|n| n.node_type == "BaseImage" && n.label == "ghcr.io/example/api:1.4.2"));
    // configMapRef → DEPENDS_ON к ConfigMap (PendingRef резолв)
    assert!(res.edges.iter().any(|e| e.label == "DEPENDS_ON"
        && e.source_node_id.ends_with("k8s:k8s/app.yaml:Deployment/api")
        && e.target_node_id.ends_with("ConfigMap/api-config")));
    // selector ↔ labels → BINDS_SELECTOR
    assert!(res.edges.iter().any(|e| e.label == "BINDS_SELECTOR"
        && e.source_node_id.ends_with("Deployment/api")
        && e.target_node_id.ends_with("ConfigMap/api-config")));
}

/// 50.5: обычный YAML (CI-workflow, application.yaml) не порождает узлов
/// кроме File; k8s-спека матчится только по каталогу.
#[test]
fn test_yaml_classifier() {
    let ex = AstCodeExtractor::new();
    for (path, content) in [
        (
            ".github/workflows/ci.yml",
            "jobs:\n  build:\n    runs-on: ubuntu-latest\n",
        ),
        (
            "config/application.yaml",
            "server:\n  port: 8080\nspring:\n  datasource: db\n",
        ),
        (
            "k8s/notes.yaml",
            "# просто заметки\nkey: value\n",
        ),
    ] {
        let mut res = AstScanResult::default();
        ex.parse_file(path, content, &mut res);
        assert!(
            res.nodes.iter().all(|n| n.node_type == "File"),
            "{}: лишние узлы {:?}",
            path,
            res.nodes
                .iter()
                .map(|n| n.node_type.clone())
                .collect::<Vec<_>>()
        );
    }
    assert!(LangSpec::for_path("k8s/app.yaml").unwrap().name == "k8s");
    assert!(LangSpec::for_path("deploy/svc.yml").unwrap().name == "k8s");
    assert!(LangSpec::for_path("chart/values.yaml").unwrap().name == "k8s");
    // случайный корневой yaml — не k8s и не compose
    assert!(LangSpec::for_path("config/application.yaml").is_none());
    assert_eq!(LangSpec::for_path("compose.yaml").unwrap().name, "compose");
    assert_eq!(LangSpec::for_path("Dockerfile").unwrap().name, "dockerfile");
    assert_eq!(LangSpec::for_path("api/Dockerfile.web").unwrap().name, "dockerfile");
    assert_eq!(LangSpec::for_path("infra/main.tf").unwrap().name, "terraform");
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
