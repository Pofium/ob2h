//! Сервис управления проектами и проектной памятью (Фаза 10, Фаза 11).

pub mod ast;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::info;

pub use ast::{AstCodeExtractor, AstScanResult};
use crate::db::models::ProjectRecord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStats {
    pub project_id: String,
    pub name: String,
    pub root_path: String,
    pub total_nodes: usize,
    pub ast_nodes: usize,
    pub total_edges: usize,
    pub god_nodes: usize,
    pub last_scanned_at: Option<String>,
}

#[derive(Clone)]
pub struct ProjectService {
    conn: Arc<Mutex<Connection>>,
    ast_extractor: Arc<AstCodeExtractor>,
}

impl ProjectService {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self {
            conn,
            ast_extractor: Arc::new(AstCodeExtractor::new()),
        }
    }

    /// Регистрирует новый проект или обновляет существующий.
    pub fn register_project(
        &self,
        id: &str,
        name: &str,
        root_path: &str,
        description: Option<&str>,
        tech_stack: Option<&[String]>,
    ) -> anyhow::Result<ProjectRecord> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let abs_path = std::fs::canonicalize(root_path)
            .unwrap_or_else(|_| PathBuf::from(root_path))
            .to_string_lossy()
            .replace('\\', "/");

        let tech_stack_json = tech_stack.map(|ts| serde_json::to_string(ts).unwrap_or_default());

        conn.execute(
            r#"
            INSERT INTO projects (id, name, root_path, description, tech_stack, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                root_path = excluded.root_path,
                description = COALESCE(excluded.description, projects.description),
                tech_stack = COALESCE(excluded.tech_stack, projects.tech_stack),
                updated_at = excluded.updated_at
            "#,
            params![id, name, abs_path, description, tech_stack_json, now, now],
        )?;

        let project = ProjectRecord {
            id: id.to_string(),
            name: name.to_string(),
            root_path: abs_path,
            description: description.map(|s| s.to_string()),
            tech_stack: tech_stack_json,
            active_branch: None,
            last_scanned_at: None,
            created_at: now.clone(),
            updated_at: now,
        };

        info!("Зарегистрирован проект '{}' ({}) по пути: {}", name, id, project.root_path);
        Ok(project)
    }

    /// Получить проект по ID.
    pub fn get_project(&self, id: &str) -> anyhow::Result<Option<ProjectRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, root_path, description, tech_stack, active_branch, last_scanned_at, created_at, updated_at FROM projects WHERE id = ?1"
        )?;

        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(ProjectRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                description: row.get(3)?,
                tech_stack: row.get(4)?,
                active_branch: row.get(5)?,
                last_scanned_at: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Список всех зарегистрированных проектов.
    pub fn list_projects(&self) -> anyhow::Result<Vec<ProjectRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, root_path, description, tech_stack, active_branch, last_scanned_at, created_at, updated_at FROM projects ORDER BY updated_at DESC"
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(ProjectRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                description: row.get(3)?,
                tech_stack: row.get(4)?,
                active_branch: row.get(5)?,
                last_scanned_at: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;

        let mut res = Vec::new();
        for r in rows {
            res.push(r?);
        }
        Ok(res)
    }

    /// Автоматическое определение проекта по рабочей директории.
    pub fn detect_project_by_path(&self, current_dir: &str) -> anyhow::Result<Option<ProjectRecord>> {
        let normalized = PathBuf::from(current_dir);
        let abs_current = std::fs::canonicalize(&normalized)
            .unwrap_or(normalized)
            .to_string_lossy()
            .replace('\\', "/");

        let projects = self.list_projects()?;
        for p in projects {
            if abs_current.starts_with(&p.root_path) {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// Сканирует кодовую базу проекта через AST и обновляет граф знаний.
    pub fn scan_project(&self, project_id: &str, custom_path: Option<&str>, _incremental: bool) -> anyhow::Result<AstScanResult> {
        let project = self.get_project(project_id)?
            .ok_or_else(|| anyhow::anyhow!("Проект с ID '{}' не найден", project_id))?;

        let scan_path = custom_path.unwrap_or(&project.root_path);
        let path_obj = Path::new(scan_path);

        if !path_obj.exists() {
            return Err(anyhow::anyhow!("Путь к проекту не существует: {}", scan_path));
        }

        info!("Начало AST-сканирования проекта '{}' по пути: {}", project_id, scan_path);

        let scan_res = self.ast_extractor.scan_directory(path_obj, None);
        let now = Utc::now().to_rfc3339();

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        // Сохраняем узлы графа
        let mut node_id_to_pk: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

        for node in &scan_res.nodes {
            tx.execute(
                r#"
                INSERT INTO graph_nodes (
                    node_id, label, node_type, description, val, created_at, updated_at,
                    project_id, file_path, line_start, line_end, provenance, is_god_node
                )
                VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5, ?6, ?7, ?8, ?9, 'ast', 0)
                ON CONFLICT(node_id) DO UPDATE SET
                    label = excluded.label,
                    node_type = excluded.node_type,
                    description = excluded.description,
                    updated_at = excluded.updated_at,
                    project_id = excluded.project_id,
                    file_path = excluded.file_path,
                    line_start = excluded.line_start,
                    line_end = excluded.line_end,
                    provenance = 'ast'
                "#,
                params![
                    node.node_id,
                    node.label,
                    node.node_type,
                    node.description,
                    now,
                    project_id,
                    node.file_path,
                    node.line_start as i64,
                    node.line_end as i64,
                ],
            )?;

            let pk: i64 = tx.query_row(
                "SELECT id FROM graph_nodes WHERE node_id = ?1",
                params![node.node_id],
                |r| r.get(0),
            )?;
            node_id_to_pk.insert(node.node_id.clone(), pk);
        }

        // Сохраняем ребра графа
        for edge in &scan_res.edges {
            let src_pk = if let Some(pk) = node_id_to_pk.get(&edge.source_node_id) {
                *pk
            } else {
                tx.query_row(
                    "SELECT id FROM graph_nodes WHERE node_id = ?1",
                    params![edge.source_node_id],
                    |r| r.get(0),
                ).optional()?.unwrap_or(0)
            };

            let dst_pk = if let Some(pk) = node_id_to_pk.get(&edge.target_node_id) {
                *pk
            } else {
                // Если узел-цель еще не существует (например, внешний модуль), создаем его
                tx.execute(
                    r#"
                    INSERT INTO graph_nodes (
                        node_id, label, node_type, description, val, created_at, updated_at,
                        project_id, provenance
                    )
                    VALUES (?1, ?1, 'ExternalModule', ?2, 1, ?3, ?3, ?4, 'ast')
                    ON CONFLICT(node_id) DO NOTHING
                    "#,
                    params![
                        edge.target_node_id,
                        format!("Внешний модуль/зависимость: {}", edge.target_node_id),
                        now,
                        project_id
                    ],
                )?;
                tx.query_row(
                    "SELECT id FROM graph_nodes WHERE node_id = ?1",
                    params![edge.target_node_id],
                    |r| r.get(0),
                ).optional()?.unwrap_or(0)
            };

            if src_pk > 0 && dst_pk > 0 {
                tx.execute(
                    r#"
                    INSERT INTO graph_edges (
                        source_id, target_id, label, weight, contexts, created_at,
                        project_id, provenance, confidence, updated_at
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'ast', 1.0, ?6)
                    ON CONFLICT(source_id, target_id, label) DO UPDATE SET
                        weight = excluded.weight,
                        contexts = excluded.contexts,
                        updated_at = excluded.updated_at,
                        project_id = excluded.project_id,
                        provenance = 'ast',
                        confidence = 1.0
                    "#,
                    params![
                        src_pk,
                        dst_pk,
                        edge.label,
                        edge.weight,
                        edge.context,
                        now,
                        project_id,
                    ],
                )?;
            }
        }

        // Обновляем время сканирования проекта
        tx.execute(
            "UPDATE projects SET last_scanned_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, project_id],
        )?;

        tx.commit()?;

        info!(
            "Сканирование проекта '{}' завершено: файлов {}, узлов {}, связей {}, строк {}",
            project_id, scan_res.files_scanned, scan_res.nodes.len(), scan_res.edges.len(), scan_res.lines_total
        );

        Ok(scan_res)
    }

    /// Статистика проекта.
    pub fn get_project_stats(&self, project_id: &str) -> anyhow::Result<ProjectStats> {
        let project = self.get_project(project_id)?
            .ok_or_else(|| anyhow::anyhow!("Проект '{}' не найден", project_id))?;

        let conn = self.conn.lock().unwrap();

        let total_nodes: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')",
            params![project_id],
            |r| r.get(0),
        ).unwrap_or(0);

        let ast_nodes: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = ?1 AND provenance = 'ast' AND (deleted_at IS NULL OR deleted_at = '')",
            params![project_id],
            |r| r.get(0),
        ).unwrap_or(0);

        let total_edges: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_edges WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')",
            params![project_id],
            |r| r.get(0),
        ).unwrap_or(0);

        let god_nodes: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_nodes WHERE project_id = ?1 AND is_god_node = 1 AND (deleted_at IS NULL OR deleted_at = '')",
            params![project_id],
            |r| r.get(0),
        ).unwrap_or(0);

        Ok(ProjectStats {
            project_id: project.id,
            name: project.name,
            root_path: project.root_path,
            total_nodes: total_nodes as usize,
            ast_nodes: ast_nodes as usize,
            total_edges: total_edges as usize,
            god_nodes: god_nodes as usize,
            last_scanned_at: project.last_scanned_at,
        })
    }
}
