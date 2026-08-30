use std::collections::HashMap;
use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GodNodeInfo {
    pub node_id: String,
    pub label: String,
    pub node_type: String,
    pub file_path: Option<String>,
    pub in_degree: usize,
    pub out_degree: usize,
    pub total_degree: usize,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectReport {
    pub project_id: String,
    pub project_name: String,
    pub root_path: String,
    pub total_nodes: usize,
    pub total_edges: usize,
    pub god_nodes: Vec<GodNodeInfo>,
    pub node_types_count: HashMap<String, usize>,
    pub top_dependencies: Vec<(String, usize)>,
    pub markdown_summary: String,
}

pub struct GraphAnalytics;

impl GraphAnalytics {
    /// Вычисляет степень связности узлов и маркирует «узлы-боги» (God Nodes) в SQLite.
    pub fn update_god_nodes(conn: &Connection, project_id: &str) -> Result<Vec<GodNodeInfo>> {
        // 1. Сбрасываем старые god nodes для проекта
        conn.execute(
            "UPDATE graph_nodes SET is_god_node = 0 WHERE project_id = ?1",
            params![project_id],
        )?;

        // 2. Считаем in-degree и out-degree для каждого узла
        let mut degree_map: HashMap<i64, (usize, usize)> = HashMap::new(); // pk -> (in_degree, out_degree)

        let mut stmt = conn.prepare(
            r#"
            SELECT source_id, target_id 
            FROM graph_edges 
            WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')
            "#,
        )?;

        let rows = stmt.query_map(params![project_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;

        for edge in rows {
            let (src, dst) = edge?;
            degree_map.entry(src).or_insert((0, 0)).1 += 1;
            degree_map.entry(dst).or_insert((0, 0)).0 += 1;
        }

        if degree_map.is_empty() {
            return Ok(Vec::new());
        }

        // Собираем узлы с их общим degree
        let mut node_scores: Vec<(i64, usize, usize, usize)> = degree_map
            .into_iter()
            .map(|(pk, (in_deg, out_deg))| (pk, in_deg, out_deg, in_deg + out_deg))
            .collect();

        node_scores.sort_by_key(|b| std::cmp::Reverse(b.3));

        // Берем топ 10% или минимум топ-5 наиболее связанных узлов
        let top_count = (node_scores.len() / 10).clamp(3, 20).min(node_scores.len());
        let god_nodes_pks: Vec<i64> = node_scores.iter().take(top_count).map(|(pk, ..)| *pk).collect();

        // Маркируем в базе
        for pk in &god_nodes_pks {
            conn.execute(
                "UPDATE graph_nodes SET is_god_node = 1 WHERE id = ?1",
                params![pk],
            )?;
        }

        // Загружаем подробную информацию о God Nodes
        let mut god_nodes_info = Vec::new();
        for (pk, in_deg, out_deg, total) in node_scores.into_iter().take(top_count) {
            let mut q_stmt = conn.prepare(
                "SELECT node_id, label, node_type, file_path, description FROM graph_nodes WHERE id = ?1",
            )?;
            if let Ok(row) = q_stmt.query_row(params![pk], |r| {
                Ok(GodNodeInfo {
                    node_id: r.get(0)?,
                    label: r.get(1)?,
                    node_type: r.get(2)?,
                    file_path: r.get(3)?,
                    in_degree: in_deg,
                    out_degree: out_deg,
                    total_degree: total,
                    description: r.get(4)?,
                })
            }) {
                god_nodes_info.push(row);
            }
        }

        Ok(god_nodes_info)
    }

    /// Генерирует исчерпывающий архитектурный дайджест проекта.
    pub fn generate_project_report(conn: &Connection, project_id: &str) -> Result<ProjectReport> {
        let (name, root_path, tech_stack): (String, String, Option<String>) = conn.query_row(
            "SELECT name, root_path, tech_stack FROM projects WHERE id = ?1",
            params![project_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;

        let god_nodes = Self::update_god_nodes(conn, project_id)?;

        let total_nodes: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')",
            params![project_id],
            |r| r.get(0),
        ).unwrap_or(0);

        let total_edges: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_edges WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')",
            params![project_id],
            |r| r.get(0),
        ).unwrap_or(0);

        // Распределение по типам узлов
        let mut node_types_count = HashMap::new();
        let mut type_stmt = conn.prepare(
            r#"
            SELECT node_type, COUNT(*) 
            FROM graph_nodes 
            WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')
            GROUP BY node_type
            "#,
        )?;
        let type_rows = type_stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?;
        for tr in type_rows {
            let (ntype, count) = tr?;
            node_types_count.insert(ntype, count);
        }

        // Топ внешних зависимостей / модулей
        let mut top_deps = Vec::new();
        let mut dep_stmt = conn.prepare(
            r#"
            SELECT target_id, COUNT(*) as cnt
            FROM graph_edges
            WHERE project_id = ?1 AND label = 'IMPORTS' AND (deleted_at IS NULL OR deleted_at = '')
            GROUP BY target_id
            ORDER BY cnt DESC
            LIMIT 10
            "#,
        )?;
        let dep_rows = dep_stmt.query_map(params![project_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize)))?;
        for dr in dep_rows {
            let (target_pk, cnt) = dr?;
            if let Ok(label) = conn.query_row("SELECT label FROM graph_nodes WHERE id = ?1", params![target_pk], |r| r.get::<_, String>(0)) {
                top_deps.push((label, cnt));
            }
        }

        // Формируем Markdown дайджест
        let mut md = String::new();
        md.push_str(&format!("# 🏛️ Архитектурный дайджест проекта: {}\n\n", name));
        md.push_str(&format!("- **ID:** `{}`\n", project_id));
        md.push_str(&format!("- **Корневой путь:** `{}`\n", root_path));
        if let Some(ts) = tech_stack {
            md.push_str(&format!("- **Стек технологий:** {}\n", ts));
        }
        md.push_str(&format!("- **Узлов в графе:** {} | **Связей:** {}\n\n", total_nodes, total_edges));

        md.push_str("## 👑 Ключевые архитектурные хабы (God Nodes)\n");
        md.push_str("Центральные структуры, модули и сервисы с максимальной связностью:\n\n");
        for gn in &god_nodes {
            let file_str = gn.file_path.as_deref().unwrap_or("внешний");
            md.push_str(&format!(
                "- **`{}`** (`{}`) — связей: {} (in: {}, out: {})\n  ↳ Файл: `{}`\n",
                gn.label, gn.node_type, gn.total_degree, gn.in_degree, gn.out_degree, file_str
            ));
        }

        md.push_str("\n## 📊 Компоненты кодовой базы\n");
        for (ntype, count) in &node_types_count {
            md.push_str(&format!("- **{}:** {}\n", ntype, count));
        }

        if !top_deps.is_empty() {
            md.push_str("\n## 📦 Наиболее используемые зависимости и модули\n");
            for (dep, cnt) in &top_deps {
                md.push_str(&format!("- `{}` (используется в {} местах)\n", dep, cnt));
            }
        }

        Ok(ProjectReport {
            project_id: project_id.to_string(),
            project_name: name,
            root_path,
            total_nodes: total_nodes as usize,
            total_edges: total_edges as usize,
            god_nodes,
            node_types_count,
            top_dependencies: top_deps,
            markdown_summary: md,
        })
    }

    /// Сборка сжатого блока `<project_context>` для системного промпта агента.
    pub fn build_project_context(conn: &Connection, project_id: &str, task_query: Option<&str>) -> Result<String> {
        let report = Self::generate_project_report(conn, project_id)?;
        let mut ctx = String::new();
        ctx.push_str(&format!("<project_context id=\"{}\">\n", project_id));
        ctx.push_str(&format!("Project: {} (Root: {})\n", report.project_name, report.root_path));
        
        ctx.push_str("Core Architecture Hubs (God Nodes):\n");
        for gn in report.god_nodes.iter().take(8) {
            let loc = gn.file_path.as_deref().unwrap_or("");
            ctx.push_str(&format!("- {} [{}] ({}) -> {} connections\n", gn.label, gn.node_type, loc, gn.total_degree));
        }

        if let Some(query) = task_query {
            ctx.push_str(&format!("\nRelevant Subsystems for Task '{}':\n", query));
            // FTS поиск релевантных узлов проекта
            let mut stmt = conn.prepare(
                r#"
                SELECT label, node_type, file_path, description 
                FROM graph_nodes 
                WHERE project_id = ?1 AND (label LIKE ?2 OR description LIKE ?2)
                LIMIT 5
                "#,
            )?;
            let query_like = format!("%{}%", query);
            let rows = stmt.query_map(params![project_id, query_like], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (lbl, ntype, fpath, desc) = row?;
                ctx.push_str(&format!("- {} [{}] in {:?}: {:?}\n", lbl, ntype, fpath, desc));
            }
        }

        ctx.push_str("</project_context>");
        Ok(ctx)
    }
}
