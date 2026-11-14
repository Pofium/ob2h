//! Ф69.3 (PLAN_v2.0): graph_golden — multi-hop кейсы на детерминированном
//! графе-фикстуре (сеется напрямую в graph_nodes/graph_edges, эквивалент
//! скана на уровне модели данных). Ожидания — node_id из
//! tests/fixtures/graph_golden.jsonl; выдача — ppr_search (Ф54).

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::params;

use ob2h::cli::bench::load_golden;
use ob2h::db::Database;
use ob2h::embedding::FakeEmbedding;
use ob2h::graph::GraphService;
use ob2h::graph::pagerank::{parse_ppr_weights, DAMPING_MAX, DEFAULT_PPR_WEIGHTS};

const PROJ: &str = "gg";

fn golden_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graph_golden.jsonl")
}

/// Граф-фикстура: run_server → parse_source → validate_token (цепочка CALLS).
fn seed(db: &Database) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name, root_path, created_at, updated_at) \
             VALUES (?1, 'GraphGolden', '/tmp/gg', '2026-01-01', '2026-01-01')",
            params![PROJ],
        )?;

        // (node_id, label, node_type, file, description)
        let nodes: [(&str, &str, &str, &str, &str); 3] = [
            (
                "fn:src/api.rs:run_server",
                "run_server",
                "Function",
                "src/api.rs",
                "Функция run_server — запуск HTTP-сервера api и роутинга",
            ),
            (
                "fn:src/parser.rs:parse_source",
                "parse_source",
                "Function",
                "src/parser.rs",
                "Функция parse_source — разбор запроса parser",
            ),
            (
                "fn:src/validate.rs:validate_token",
                "validate_token",
                "Function",
                "src/validate.rs",
                "Функция validate_token — проверка токена",
            ),
        ];
        for (i, (nid, label, ntype, file, desc)) in nodes.iter().enumerate() {
            conn.execute(
                "INSERT INTO graph_nodes (id, node_id, label, node_type, description, file_path, line_start, project_id, provenance, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 'ast', '2026-01-01', '2026-01-01')",
                params![(i + 1) as i64, nid, label, ntype, desc, file, PROJ],
            )?;
        }

        let edges: [(i64, i64, &str); 2] = [(1, 2, "CALLS"), (2, 3, "CALLS")];
        for (s, t, label) in edges {
            conn.execute(
                "INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'ast', '2026-01-01')",
                params![s, t, label, PROJ],
            )?;
        }
        Ok(())
    })
    .expect("seed graph");
}

#[tokio::test]
async fn graph_golden_multi_hop_reachable() {
    let cases = load_golden(&golden_path()).expect("graph_golden загружается");
    assert_eq!(cases.len(), 4, "4 кейса: прямой сид ×2, 1-hop, 2-hop");

    let db = Database::in_memory().expect("db in memory");
    seed(&db);
    let graph = GraphService::new(db, Arc::new(FakeEmbedding::new(384)));
    let damping = DAMPING_MAX;
    let weights = parse_ppr_weights(DEFAULT_PPR_WEIGHTS);

    // pk → строковый node_id (как в mode=code bench)
    let id_map: std::collections::HashMap<i64, String> =
        graph.all_node_ids(PROJ).expect("all_node_ids").into_iter().collect();

    let _ = graph
        .ppr_search("прогрев", Some(PROJ), 5, damping, &weights)
        .await;

    for case in &cases {
        let hits = graph
            .ppr_search(&case.query, Some(PROJ), 10, damping, &weights)
            .await
            .expect("ppr_search");
        let ranked: Vec<String> = hits
            .iter()
            .filter_map(|h| id_map.get(&h.node_id).cloned())
            .collect();
        assert!(!ranked.is_empty(), "case `{}`: пустая выдача", case.query);

        let expected: HashSet<&str> = case.expect_keys.iter().map(|s| s.as_str()).collect();
        let hits10 = ranked.iter().take(10).filter(|r| expected.contains(r.as_str())).count();
        assert_eq!(
            hits10,
            expected.len(),
            "case `{}` ({}): ожидались {:?}, выдача: {:?}",
            case.query,
            case.note,
            case.expect_keys,
            ranked
        );
    }
}
