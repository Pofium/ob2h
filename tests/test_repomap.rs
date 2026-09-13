//! Ф37 (трек C): repo-map под token budget — тесты на сеяном графе (эквивалент
//! fixture-репо: File/ExternalModule узлы + IMPORTS + DEFINES, как делает AST-сканер).

use ob2h::db::Database;
use ob2h::graph::repomap::{build_repo_map, TOKEN_CHARS};

const PROJ: &str = "map";

/// Сеем граф: hub-файл (много связей), два хвостовых файла, общий внешний модуль.
fn seed_graph(db: &Database) {
    db.with_conn(|conn| {
        conn.execute_batch(
            "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('map', 'map', '/tmp/map', '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z');

             INSERT INTO graph_nodes (node_id, label, node_type, project_id, file_path, description, is_god_node, created_at, updated_at)
             VALUES
               ('f:hub',  'src/hub.rs',   'File', 'map', 'src/hub.rs',   '', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('f:a',    'src/a.rs',     'File', 'map', 'src/a.rs',     '', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('f:b',    'src/b.rs',     'File', 'map', 'src/b.rs',     '', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('f:tail', 'src/tail.rs',  'File', 'map', 'src/tail.rs',  '', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('m:serde','module:serde', 'ExternalModule', 'map', NULL, '', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('m:util', 'module:util',  'ExternalModule', 'map', NULL, '', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z');

             -- hub/a/b импортируют общий модуль serde (ко-импортная связность),
             -- tail — только util (не связан с остальными)
             INSERT INTO graph_edges (source_id, target_id, label, weight, project_id, provenance, created_at)
             SELECT n1.id, n2.id, 'IMPORTS', 1.0, 'map', 'ast_scan', '2026-09-13T10:00:00Z'
             FROM graph_nodes n1, graph_nodes n2
             WHERE (n1.label='src/hub.rs'  AND n2.label='module:serde')
                OR (n1.label='src/a.rs'    AND n2.label='module:serde')
                OR (n1.label='src/b.rs'    AND n2.label='module:serde')
                OR (n1.label='src/tail.rs' AND n2.label='module:util');

             -- Символы (до DEFINES-рёбер — они ссылаются на символы)
             INSERT INTO graph_nodes (node_id, label, node_type, project_id, file_path, description, is_god_node, created_at, updated_at)
             VALUES
               ('s:hubfn',    'run_pipeline', 'Function', 'map', 'src/hub.rs',
                'Функция `run_pipeline` (pub fn run_pipeline(cfg: &Config) -> Result<()> {) в src/hub.rs', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('s:hubstruct','Config',       'Struct',   'map', 'src/hub.rs',
                'Структура `Config` (pub struct Config {) в src/hub.rs', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('s:afn',      'load_input',   'Function', 'map', 'src/a.rs',
                'Функция `load_input` (fn load_input(path: &str) -> Vec<u8> {) в src/a.rs', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('s:bfn',      'write_out',    'Function', 'map', 'src/b.rs',
                'Функция `write_out` (fn write_out(data: &[u8]) {) в src/b.rs', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z'),
               ('s:tailfn',   'legacy_hook',  'Function', 'map', 'src/tail.rs',
                'Функция `legacy_hook` (fn legacy_hook() {) в src/tail.rs', 0, '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z');

             -- DEFINES: hub несёт 2 символа, a/b — по одному, tail — один мёртвый
             INSERT INTO graph_edges (source_id, target_id, label, weight, project_id, provenance, created_at)
             SELECT fn.id, s.id, 'DEFINES', 1.0, 'map', 'ast_scan', '2026-09-13T10:00:00Z'
             FROM graph_nodes fn, graph_nodes s
             WHERE (fn.label='src/hub.rs' AND s.node_id IN ('s:hubfn','s:hubstruct'))
                OR (fn.label='src/a.rs'   AND s.node_id='s:afn')
                OR (fn.label='src/b.rs'   AND s.node_id='s:bfn')
                OR (fn.label='src/tail.rs' AND s.node_id='s:tailfn');
             ",
        )
    }).unwrap();
}

fn repo_map(db: &Database, query: Option<&str>, tokens: usize) -> String {
    db.with_conn(|conn| build_repo_map(conn, PROJ, query, tokens))
        .unwrap()
}

#[test]
fn god_nodes_first_and_budget_respected() {
    let db = Database::in_memory().unwrap();
    seed_graph(&db);

    // Большой бюджет: все 4 файла, hub (наибольшая связность) — первым блоком.
    let map = repo_map(&db, None, 8192);
    assert!(map.starts_with("<repo_map"));
    let hub_pos = map.find("## src/hub.rs").expect("hub-файл в карте");
    let tail_pos = map.find("## src/tail.rs").expect("tail-файл в карте");
    assert!(hub_pos < tail_pos, "God Node (hub) раньше хвоста");
    assert!(
        map.contains("pub fn run_pipeline"),
        "сигнатура извлечена из описания"
    );
    assert!(map.contains("pub struct Config"));
    assert!(map.contains("used="), "отчёт об использованном бюджете");

    // Крошечный бюджет (30 токенов = 120 символов): карта уложена, usage ≤ бюджета.
    for budget in [8usize, 30, 128, 2048, 4096, 8192] {
        let m = repo_map(&db, None, budget);
        let body_chars = m.len();
        assert!(
            body_chars <= budget * TOKEN_CHARS + 200, // +строка used/…-хвост
            "бюджет {budget}tok превышен: {body_chars} символов"
        );
    }
}

#[test]
fn query_seeds_focus_relevant_files() {
    let db = Database::in_memory().unwrap();
    seed_graph(&db);
    // query=tail: tail-файл получает сид; tail изолирован в графе файлов (у util
    // один файл — ко-импортных пар нет), поэтому карта честно содержит только его.
    let m = repo_map(&db, Some("tail"), 8192);
    assert!(m.find("## src/tail.rs").is_some(), "tail в карте");
    assert!(
        m.find("src/hub.rs").is_none(),
        "изолированный от сида файл не должен получать массу PPR"
    );
    // Глобальная карта содержит оба.
    let m2 = repo_map(&db, None, 8192);
    assert!(m2.find("## src/hub.rs").is_some() && m2.find("## src/tail.rs").is_some());
}

#[test]
fn deterministic_for_same_db() {
    let db = Database::in_memory().unwrap();
    seed_graph(&db);
    let a = repo_map(&db, None, 4096);
    let b = repo_map(&db, None, 4096);
    assert_eq!(a, b, "карта детерминирована на той же БД");
}

#[test]
fn empty_project_is_honest() {
    let db = Database::in_memory().unwrap();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('empty', 'e', '/tmp/e', '2026-09-13T10:00:00Z', '2026-09-13T10:00:00Z')",
            [],
        )
    })
    .unwrap();
    let m = db
        .with_conn(|conn| build_repo_map(conn, "empty", None, 4096))
        .unwrap();
    assert!(
        m.contains("нет сканированных файлов"),
        "честное сообщение вместо пустой карты"
    );
}
