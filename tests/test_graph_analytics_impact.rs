//! Интеграционный тест графовой архитектурной аналитики:
//! - Детектор циклических зависимостей (Tarjan's SCC),
//! - Анализ радиуса изменений (Blast Radius / `project_impact`),
//! - Метрики стабильности компонентов (Robert C. Martin: Ca, Ce, I).

use std::sync::Arc;
use tempfile::tempdir;
use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::graph::analytics::{GraphAnalytics, RiskLevel};
use ob2h::init_app;
use ob2h::mcp::McpServer;

#[test]
fn test_circular_dependency_detection_tarjan() -> anyhow::Result<()> {
    let db = Database::in_memory()?;

    // 1. Создаем граф с циклом A -> B -> C -> A и изолированным D
    db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('cycle_proj', 'Cycle Project', '/tmp', '2026-01-01', '2026-01-01')", [])?;
        conn.execute(
            r#"
            INSERT INTO graph_nodes (id, node_id, label, node_type, file_path, project_id, provenance, created_at, updated_at)
            VALUES 
            (1, 'node_a', 'module_a', 'Module', 'src/a.rs', 'cycle_proj', 'ast', '2026-01-01', '2026-01-01'),
            (2, 'node_b', 'module_b', 'Module', 'src/b.rs', 'cycle_proj', 'ast', '2026-01-01', '2026-01-01'),
            (3, 'node_c', 'module_c', 'Module', 'src/c.rs', 'cycle_proj', 'ast', '2026-01-01', '2026-01-01'),
            (4, 'node_d', 'module_d', 'Module', 'src/d.rs', 'cycle_proj', 'ast', '2026-01-01', '2026-01-01');
            "#,
            [],
        )?;

        // Ребра цикла: 1 -> 2 -> 3 -> 1, и однонаправленное 3 -> 4
        conn.execute(
            r#"
            INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at)
            VALUES 
            (1, 2, 'IMPORTS', 'cycle_proj', 'ast', '2026-01-01'),
            (2, 3, 'IMPORTS', 'cycle_proj', 'ast', '2026-01-01'),
            (3, 1, 'IMPORTS', 'cycle_proj', 'ast', '2026-01-01'),
            (3, 4, 'IMPORTS', 'cycle_proj', 'ast', '2026-01-01');
            "#,
            [],
        )?;
        Ok(())
    })?;

    let cycles = db.with_conn(|conn| GraphAnalytics::find_circular_dependencies(conn, "cycle_proj"))?;
    assert_eq!(cycles.len(), 1, "Должен быть найден ровно один цикл");
    assert_eq!(cycles[0].length, 3, "Длина цикла должна быть 3 (A, B, C)");
    let joined = cycles[0].nodes.join(" ");
    assert!(joined.contains("module_a"));
    assert!(joined.contains("module_b"));
    assert!(joined.contains("module_c"));
    assert!(!joined.contains("module_d"));

    Ok(())
}

#[tokio::test]
async fn test_blast_radius_and_mcp_impact() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.ensure_dirs()?;

    let ctx = init_app(settings)?;
    let server = Arc::new(McpServer::new(ctx.clone()));

    // Создаем цепочку: api_handler -> auth_service -> user_repo -> db_query (target, GodNode)
    ctx.db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('impact_proj', 'Impact Proj', '/app', '2026-01-01', '2026-01-01')", [])?;
        conn.execute(
            r#"
            INSERT INTO graph_nodes (id, node_id, label, node_type, file_path, project_id, provenance, is_god_node, created_at, updated_at)
            VALUES 
            (10, 'db_q', 'db_query', 'Function', 'src/db.rs', 'impact_proj', 'ast', 1, '2026-01-01', '2026-01-01'),
            (20, 'u_repo', 'user_repo', 'Struct', 'src/user.rs', 'impact_proj', 'ast', 0, '2026-01-01', '2026-01-01'),
            (30, 'a_svc', 'auth_service', 'Struct', 'src/auth.rs', 'impact_proj', 'ast', 0, '2026-01-01', '2026-01-01'),
            (40, 'api_h', 'api_handler', 'Function', 'src/api.rs', 'impact_proj', 'ast', 0, '2026-01-01', '2026-01-01'),
            (50, 'other', 'unrelated_util', 'Function', 'src/util.rs', 'impact_proj', 'ast', 0, '2026-01-01', '2026-01-01');
            "#,
            [],
        )?;

        // Рёбра вызовов
        conn.execute(
            r#"
            INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at)
            VALUES 
            (20, 10, 'CALLS', 'impact_proj', 'ast', '2026-01-01'),
            (30, 20, 'CALLS', 'impact_proj', 'ast', '2026-01-01'),
            (40, 30, 'CALLS', 'impact_proj', 'ast', '2026-01-01');
            "#,
            [],
        )?;
        Ok(())
    })?;

    // 1. Тестируем прямой метод analyze_impact
    let impact = ctx.db.with_conn(|conn| {
        GraphAnalytics::analyze_impact(conn, "impact_proj", "db_query", 3)
    })?;

    assert_eq!(impact.target_symbol, "db_query");
    assert_eq!(impact.affected_nodes.len(), 3);
    assert_eq!(impact.risk_level, RiskLevel::High, "Поскольку db_query является God Node, риск должен быть High");
    assert!(impact.risk_factors.iter().any(|f| f.contains("God Node")));

    // Проверяем глубину
    let depths: Vec<(String, usize)> = impact.affected_nodes.iter().map(|n| (n.label.clone(), n.depth)).collect();
    assert_eq!(depths, vec![
        ("user_repo".to_string(), 1),
        ("auth_service".to_string(), 2),
        ("api_handler".to_string(), 3),
    ]);

    // 2. Тестируем ограничение глубины max_depth = 1
    let shallow = ctx.db.with_conn(|conn| {
        GraphAnalytics::analyze_impact(conn, "impact_proj", "db_query", 1)
    })?;
    assert_eq!(shallow.affected_nodes.len(), 1);
    assert_eq!(shallow.affected_nodes[0].label, "user_repo");

    // 3. Тестируем метрики связности пакетов
    let metrics = ctx.db.with_conn(|conn| {
        GraphAnalytics::compute_coupling_metrics(conn, "impact_proj")
    })?;
    assert!(!metrics.is_empty());
    // src/db.rs только вызывается другими, сам никого не вызывает: Ca > 0, Ce = 0 => Instability = 0.0 (Ядро)
    let db_metric = metrics.iter().find(|m| m.component == "src/db.rs").expect("db.rs metric");
    assert_eq!(db_metric.efferent_ce, 0);
    assert_eq!(db_metric.instability, 0.0);
    assert_eq!(db_metric.category, "Стабильный (Ядро)");

    // 4. Тестируем вызов через MCP-инструмент project_impact
    let mcp_out = server.call_tool("project_impact", serde_json::json!({
        "symbol_or_path": "db_query",
        "id": "impact_proj",
        "depth": 3
    })).await;

    assert!(mcp_out.contains("💥 Анализ радиуса изменений (Blast Radius)"));
    assert!(mcp_out.contains("🔴 HIGH"));
    assert!(mcp_out.contains("user_repo"));
    assert!(mcp_out.contains("auth_service"));
    assert!(mcp_out.contains("api_handler"));

    // 5. Тестируем вызов project_report, проверяем интеграцию циклов и стабильности
    let report_out = server.call_tool("project_report", serde_json::json!({
        "id": "impact_proj"
    })).await;
    assert!(report_out.contains("Циклические зависимости"));
    assert!(report_out.contains("Архитектурная стабильность"));

    Ok(())
}

#[test]
fn test_blast_radius_isolated_node() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('iso_proj', 'Iso', '/app', '2026-01-01', '2026-01-01')", [])?;
        conn.execute(
            "INSERT INTO graph_nodes (id, node_id, label, node_type, file_path, project_id, provenance, is_god_node, created_at, updated_at)
             VALUES (1, 'iso_fn', 'standalone_func', 'Function', 'src/util.rs', 'iso_proj', 'ast', 0, '2026-01-01', '2026-01-01')",
            [],
        )?;
        Ok(())
    })?;

    let impact = db.with_conn(|conn| GraphAnalytics::analyze_impact(conn, "iso_proj", "standalone_func", 3))?;
    assert_eq!(impact.risk_level, RiskLevel::Low);
    assert_eq!(impact.affected_nodes.len(), 0);
    assert!(impact.markdown_summary.contains("Зависимых компонентов не обнаружено"));

    Ok(())
}

#[test]
fn test_blast_radius_unknown_symbol() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('p', 'P', '/app', '2026-01-01', '2026-01-01')", [])
    })?;

    let impact = db.with_conn(|conn| GraphAnalytics::analyze_impact(conn, "p", "non_existent_function", 3))?;
    assert_eq!(impact.risk_level, RiskLevel::Low);
    assert!(impact.markdown_summary.contains("не найден в графе проекта"));

    Ok(())
}

#[test]
fn test_circular_dependency_self_loop() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('loop_proj', 'Loop', '/app', '2026-01-01', '2026-01-01')", [])?;
        conn.execute(
            "INSERT INTO graph_nodes (id, node_id, label, node_type, file_path, project_id, provenance, created_at, updated_at)
             VALUES (1, 'self_loop_node', 'recursive_fn', 'Function', 'src/rec.rs', 'loop_proj', 'ast', '2026-01-01', '2026-01-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at)
             VALUES (1, 1, 'CALLS', 'loop_proj', 'ast', '2026-01-01')",
            [],
        )?;
        Ok(())
    })?;

    let cycles = db.with_conn(|conn| GraphAnalytics::find_circular_dependencies(conn, "loop_proj"))?;
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0].length, 1);
    assert!(cycles[0].nodes[0].contains("recursive_fn"));

    Ok(())
}

#[test]
fn test_circular_dependency_acyclic() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('dag_proj', 'DAG', '/app', '2026-01-01', '2026-01-01')", [])?;
        conn.execute(
            "INSERT INTO graph_nodes (id, node_id, label, node_type, file_path, project_id, provenance, created_at, updated_at)
             VALUES 
             (1, 'n1', 'a', 'Module', 'src/a.rs', 'dag_proj', 'ast', '2026-01-01', '2026-01-01'),
             (2, 'n2', 'b', 'Module', 'src/b.rs', 'dag_proj', 'ast', '2026-01-01', '2026-01-01'),
             (3, 'n3', 'c', 'Module', 'src/c.rs', 'dag_proj', 'ast', '2026-01-01', '2026-01-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at)
             VALUES 
             (1, 2, 'IMPORTS', 'dag_proj', 'ast', '2026-01-01'),
             (2, 3, 'IMPORTS', 'dag_proj', 'ast', '2026-01-01')",
            [],
        )?;
        Ok(())
    })?;

    let cycles = db.with_conn(|conn| GraphAnalytics::find_circular_dependencies(conn, "dag_proj"))?;
    assert!(cycles.is_empty(), "В ацикличном графе циклов быть не должно");

    Ok(())
}

#[test]
fn test_component_metrics_unstable_leaf() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('leaf_proj', 'Leaf', '/app', '2026-01-01', '2026-01-01')", [])?;
        conn.execute(
            "INSERT INTO graph_nodes (id, node_id, label, node_type, file_path, project_id, provenance, created_at, updated_at)
             VALUES 
             (1, 'ui_n', 'main_window', 'Function', 'src/ui.rs', 'leaf_proj', 'ast', '2026-01-01', '2026-01-01'),
             (2, 'core_n', 'core_func', 'Function', 'src/core.rs', 'leaf_proj', 'ast', '2026-01-01', '2026-01-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at)
             VALUES (1, 2, 'CALLS', 'leaf_proj', 'ast', '2026-01-01')",
            [],
        )?;
        Ok(())
    })?;

    let metrics = db.with_conn(|conn| GraphAnalytics::compute_coupling_metrics(conn, "leaf_proj"))?;
    let ui_metric = metrics.iter().find(|m| m.component == "src/ui.rs").expect("ui.rs metric");
    assert_eq!(ui_metric.afferent_ca, 0);
    assert_eq!(ui_metric.efferent_ce, 1);
    assert_eq!(ui_metric.instability, 1.0);
    assert_eq!(ui_metric.category, "Нестабильный (Лист)");

    Ok(())
}

#[test]
fn test_component_metrics_balanced() -> anyhow::Result<()> {
    let db = Database::in_memory()?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES ('bal_proj', 'Bal', '/app', '2026-01-01', '2026-01-01')", [])?;
        conn.execute(
            "INSERT INTO graph_nodes (id, node_id, label, node_type, file_path, project_id, provenance, created_at, updated_at)
             VALUES 
             (1, 'top', 'top_fn', 'Function', 'src/top.rs', 'bal_proj', 'ast', '2026-01-01', '2026-01-01'),
             (2, 'mid', 'mid_fn', 'Function', 'src/mid.rs', 'bal_proj', 'ast', '2026-01-01', '2026-01-01'),
             (3, 'bot', 'bot_fn', 'Function', 'src/bot.rs', 'bal_proj', 'ast', '2026-01-01', '2026-01-01')",
            [],
        )?;
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, project_id, provenance, created_at)
             VALUES 
             (1, 2, 'CALLS', 'bal_proj', 'ast', '2026-01-01'),
             (2, 3, 'CALLS', 'bal_proj', 'ast', '2026-01-01')",
            [],
        )?;
        Ok(())
    })?;

    let metrics = db.with_conn(|conn| GraphAnalytics::compute_coupling_metrics(conn, "bal_proj"))?;
    let mid_metric = metrics.iter().find(|m| m.component == "src/mid.rs").expect("mid.rs metric");
    assert_eq!(mid_metric.afferent_ca, 1);
    assert_eq!(mid_metric.efferent_ce, 1);
    assert_eq!(mid_metric.instability, 0.5);
    assert_eq!(mid_metric.category, "Сбалансированный");

    Ok(())
}
