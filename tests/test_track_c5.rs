//! Ф40 (трек C): communities (зоны), framework edges (ROUTE/QUERIES_TABLE),
//! связка graph↔memory (meta.code_symbols), dead-code учитывает ROUTE.

use ob2h::db::Database;
use ob2h::graph::callpath;
use ob2h::project::ast::{AstCodeExtractor, AstScanResult};

const PROJ: &str = "c5";

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[test]
fn route_edges_detected_and_excluded_from_dead_code() {
    let ex = AstCodeExtractor::new();
    let mut scan = AstScanResult::default();
    ex.parse_file(
        "src/api.rs",
        r#"use axum::routing::get;

#[get("/users")]
pub async fn list_users() -> &'static str { "ok" }

fn internal_helper() -> u8 { 1 }
"#,
        &mut scan,
    );

    // ROUTE найден (INFERRED): list_users -> route:GET /users
    let route = scan
        .edges
        .iter()
        .find(|e| e.label == "ROUTE")
        .expect("ROUTE-ребро найдено");
    assert_eq!(route.source_node_id, "fn:src/api.rs:list_users");
    assert_eq!(route.target_node_id, "route:GET /users");
    assert_eq!(route.provenance, "INFERRED");

    // DB: scan_project кладёт ROUTE, dead-code НЕ считает list_users мёртвым
    let db = Database::in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/api.rs"),
        "#[get(\"/users\")]\npub async fn list_users() -> &'static str { \"ok\" }\n\nfn internal_helper() -> u8 { 1 }\n",
    )
    .unwrap();
    let svc = ob2h::project::ProjectService::new(db.conn_arc());
    svc.register_project(PROJ, PROJ, dir.path().to_str().unwrap(), None, None)
        .unwrap();
    svc.scan_project(PROJ, None, false).unwrap();

    db.with_conn(|conn| {
        let route_nodes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_nodes WHERE project_id=?1 AND node_type='Route'",
                rusqlite::params![PROJ],
                |r| r.get(0),
            )
            .unwrap();
        assert!(route_nodes >= 1, "route-узел создан в БД");

        let dead = callpath::dead_code(conn, PROJ).unwrap();
        let dead_labels: Vec<&str> = dead.iter().map(|d| d.symbol.label.as_str()).collect();
        assert!(
            dead_labels.contains(&"internal_helper"),
            "функция без вызовов и без маршрута — мёртвая: {:?}",
            dead_labels
        );
        assert!(
            !dead_labels.contains(&"list_users"),
            "маршрутный handler — живой entrypoint (Ф40.4): {:?}",
            dead_labels
        );
        Ok::<(), rusqlite::Error>(())
    })
    .unwrap();
}

#[test]
fn fastapi_route_and_tablename_detected() {
    let ex = AstCodeExtractor::new();
    let mut scan = AstScanResult::default();
    ex.parse_file(
        "app/api.py",
        "@app.get(\"/items\")\ndef list_items():\n    return []\n\nclass User(Base):\n    __tablename__ = \"users\"\n",
        &mut scan,
    );
    assert!(
        scan.edges
            .iter()
            .any(|e| e.label == "ROUTE" && e.target_node_id == "route:GET /items"),
        "FastAPI-маршрут найден: {:?}",
        scan.edges
    );
    assert!(
        scan.edges
            .iter()
            .any(|e| e.label == "QUERIES_TABLE" && e.target_node_id == "table:users"),
        "QUERIES_TABLE найден: {:?}",
        scan.edges
    );
}

#[test]
fn zones_detected_on_two_module_fixture() {
    // Два плотных кластера, слабо связанных между собой
    let db = Database::in_memory().unwrap();
    let now = now();
    db.with_conn(|conn| {
        conn.execute_batch(&format!(
            "INSERT INTO projects (id, name, root_path, created_at, updated_at)
             VALUES ('{PROJ}', '{PROJ}', '/tmp/c5', '{now}', '{now}');
             INSERT INTO graph_nodes (node_id, label, node_type, project_id, created_at, updated_at)
             VALUES
               ('a1','fa1','Function','{PROJ}','{now}','{now}'),
               ('a2','fa2','Function','{PROJ}','{now}','{now}'),
               ('a3','fa3','Function','{PROJ}','{now}','{now}'),
               ('b1','fb1','Function','{PROJ}','{now}','{now}'),
               ('b2','fb2','Function','{PROJ}','{now}','{now}'),
               ('b3','fb3','Function','{PROJ}','{now}','{now}');
             INSERT INTO graph_edges (source_id, target_id, label, weight, project_id, created_at)
             VALUES
               (1,2,'CALLS',1.0,'{PROJ}','{now}'), (2,3,'CALLS',1.0,'{PROJ}','{now}'), (3,1,'CALLS',1.0,'{PROJ}','{now}'),
               (4,5,'CALLS',1.0,'{PROJ}','{now}'), (5,6,'CALLS',1.0,'{PROJ}','{now}'), (6,4,'CALLS',1.0,'{PROJ}','{now}'),
               (1,4,'CALLS',0.1,'{PROJ}','{now}');
             "
        ))
    })
    .unwrap();

    db.with_conn(|conn| {
        let report = ob2h::graph::communities::detect_zones(conn, PROJ, 2).unwrap();
        assert!(
            report.modularity_q > 0.1,
            "два плотных кластера дают Q>0.1: Q={:.3} zones={:?}",
            report.modularity_q,
            report.zones
        );
        assert_eq!(
            report.zones.len(),
            2,
            "ожидается 2 зоны: {:?}",
            report.zones
        );
        // секция формируется
        let md = ob2h::graph::communities::format_zones(&report);
        assert!(md.contains("Зоны (communities"));
        Ok::<(), rusqlite::Error>(())
    })
    .unwrap();
}

#[test]
fn memory_save_links_code_symbols() {
    let db = Database::in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/svc.rs"),
        "pub struct PersonalizedPagerank;\n\npub fn personalized_pagerank() {}\n",
    )
    .unwrap();
    let svc = ob2h::project::ProjectService::new(db.conn_arc());
    svc.register_project(PROJ, PROJ, dir.path().to_str().unwrap(), None, None)
        .unwrap();
    svc.scan_project(PROJ, None, false).unwrap();

    // is_god_node у personalized_pagerank — линк приоритетен
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE graph_nodes SET is_god_node = 1 WHERE label = 'personalized_pagerank'",
            [],
        )
    })
    .unwrap();

    // memory_save через MCP-обработчик проверяется интеграционно; здесь —
    // уровень данные: mentioned_symbols находит упомянутые символы
    db.with_conn(|conn| {
        let syms = ob2h::graph::communities::mentioned_symbols(
            conn,
            PROJ,
            "PPR ранжирование вынесено в personalized_pagerank; решение записано",
            8,
        )
        .unwrap();
        assert!(
            syms.contains(&"personalized_pagerank".to_string()),
            "упомянутый символ найден: {:?}",
            syms
        );
        // meta-линк пишется напрямую (как в MCP-обработчике memory_save)
        let meta = serde_json::json!({ "code_symbols": syms });
        conn.execute(
            "INSERT INTO memories (key, content, category, importance, created_at, updated_at, project_id, meta)
             VALUES ('ppr-adr', 'ADR: PPR', 'adr', 0.8, ?1, ?1, ?2, ?3)",
            rusqlite::params![now(), PROJ, meta.to_string()],
        )
        .unwrap();
        let stored: String = conn
            .query_row("SELECT meta FROM memories WHERE key='ppr-adr'", [], |r| r.get(0))
            .unwrap();
        assert!(stored.contains("code_symbols"));
        Ok::<(), rusqlite::Error>(())
    })
    .unwrap();
}
