//! Сервис управления проектами и проектной памятью (Фаза 10, Фаза 11).

pub mod ast;
pub mod hooks;
pub mod watcher;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::info;

pub use ast::{AstCodeExtractor, AstScanResult};
pub use hooks::install_git_hooks;
pub use watcher::ProjectWatcher;
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

    /// Возвращает мапу известных файлов проекта rel_path -> sha256.
    pub fn get_known_files(&self, project_id: &str) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT rel_path, sha256 FROM project_files WHERE project_id = ?1"
        )?;
        let rows = stmt.query_map(params![project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (p, h) = r?;
            map.insert(p, h);
        }
        Ok(map)
    }

    /// Автоматически определяет проект по рабочей директории или регистрирует его "на лету" (Zero-Config).
    pub fn auto_register_or_detect(&self, start_dir: &Path) -> anyhow::Result<ProjectRecord> {
        let root = find_project_root(start_dir);
        let abs_root = std::fs::canonicalize(&root)
            .unwrap_or(root.clone())
            .to_string_lossy()
            .replace('\\', "/");

        // 1. Проверяем, существует ли уже проект с таким root_path
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, root_path, description, tech_stack, active_branch, last_scanned_at, created_at, updated_at FROM projects WHERE root_path = ?1"
        )?;
        let mut rows = stmt.query(params![abs_root])?;
        if let Some(row) = rows.next()? {
            return Ok(ProjectRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                description: row.get(3)?,
                tech_stack: row.get(4)?,
                active_branch: row.get(5)?,
                last_scanned_at: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            });
        }
        drop(rows);
        drop(stmt);
        drop(conn);

        // 2. Не найден - извлекаем метаданные и регистрируем
        let (name, tech_stack, description) = detect_manifest_metadata(&root);

        // Генерируем уникальный slug для ID
        let mut slug = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
            .collect::<String>();
        while slug.contains("--") {
            slug = slug.replace("--", "-");
        }
        let slug = slug.trim_matches('-').to_string();
        let base_id = if slug.is_empty() { "project".to_string() } else { slug };
        let mut project_id = base_id.clone();
        let mut counter = 1;
        while self.get_project(&project_id)?.is_some() {
            project_id = format!("{}-{}", base_id, counter);
            counter += 1;
        }

        let mut project = self.register_project(
            &project_id,
            &name,
            &abs_root,
            description.as_deref(),
            Some(&tech_stack),
        )?;

        // Детектируем активную ветку git, если есть
        let head_file = root.join(".git").join("HEAD");
        if head_file.exists() {
            if let Ok(head_content) = std::fs::read_to_string(&head_file) {
                let branch = head_content.trim().trim_start_matches("ref: refs/heads/").to_string();
                if !branch.is_empty() {
                    let conn = self.conn.lock().unwrap();
                    let _ = conn.execute(
                        "UPDATE projects SET active_branch = ?1 WHERE id = ?2",
                        params![branch, project.id],
                    );
                    project.active_branch = Some(branch);
                }
            }
        }

        info!("Zero-Config: автоматически зарегистрирован проект '{}' ({}) по пути {}", name, project.id, abs_root);
        Ok(project)
    }

    /// Сканирует кодовую базу проекта через AST и обновляет граф знаний.
    pub fn scan_project(&self, project_id: &str, custom_path: Option<&str>, incremental: bool) -> anyhow::Result<AstScanResult> {
        let project = self.get_project(project_id)?
            .ok_or_else(|| anyhow::anyhow!("Проект с ID '{}' не найден", project_id))?;

        let scan_path = custom_path.unwrap_or(&project.root_path);
        let path_obj = Path::new(scan_path);

        if !path_obj.exists() {
            return Err(anyhow::anyhow!("Путь к проекту не существует: {}", scan_path));
        }

        info!("Начало AST-сканирования проекта '{}' (incremental={}) по пути: {}", project_id, incremental, scan_path);

        let known_hashes = if incremental {
            self.get_known_files(project_id)?
        } else {
            std::collections::HashMap::new()
        };

        let scan_res = self.ast_extractor.scan_directory(path_obj, if incremental { Some(&known_hashes) } else { None });
        let now = Utc::now().to_rfc3339();

        // Поиск удалённых файлов
        let deleted_files: Vec<String> = if incremental {
            known_hashes
                .keys()
                .filter(|k| !scan_res.file_hashes.contains_key(*k))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // Если ничего не изменилось и нет удалённых файлов
        if incremental && scan_res.files_scanned == 0 && deleted_files.is_empty() {
            info!("Инкрементальный скан: изменений не обнаружено для проекта '{}'", project_id);
            return Ok(scan_res);
        }

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        // Если не инкрементальный скан - очищаем старые ast-данные проекта
        if !incremental {
            tx.execute(
                "DELETE FROM graph_nodes WHERE project_id = ?1 AND provenance = 'ast'",
                params![project_id],
            )?;
            tx.execute(
                "DELETE FROM graph_edges WHERE project_id = ?1 AND provenance = 'ast'",
                params![project_id],
            )?;
            tx.execute(
                "DELETE FROM project_files WHERE project_id = ?1",
                params![project_id],
            )?;
        } else {
            // Удаляем узлы удалённых файлов
            for del_file in &deleted_files {
                tx.execute(
                    "DELETE FROM graph_nodes WHERE project_id = ?1 AND file_path = ?2",
                    params![project_id, del_file],
                )?;
                tx.execute(
                    "DELETE FROM project_files WHERE project_id = ?1 AND rel_path = ?2",
                    params![project_id, del_file],
                )?;
            }

            // Для изменённых файлов удаляем старые узлы перед записью новых
            for rel_path in scan_res.file_meta.keys() {
                tx.execute(
                    "DELETE FROM graph_nodes WHERE project_id = ?1 AND file_path = ?2",
                    params![project_id, rel_path],
                )?;
            }
        }

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

        // Сохраняем/обновляем хэши файлов в таблице project_files
        for (rel_path, hash) in &scan_res.file_hashes {
            let (file_size, lines_count) = scan_res.file_meta.get(rel_path).copied().unwrap_or((0, 0));
            tx.execute(
                r#"
                INSERT INTO project_files (project_id, rel_path, sha256, file_size, lines_count, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, rel_path) DO UPDATE SET
                    sha256 = excluded.sha256,
                    file_size = CASE WHEN excluded.file_size > 0 THEN excluded.file_size ELSE project_files.file_size END,
                    lines_count = CASE WHEN excluded.lines_count > 0 THEN excluded.lines_count ELSE project_files.lines_count END,
                    updated_at = excluded.updated_at
                "#,
                params![project_id, rel_path, hash, file_size as i64, lines_count as i64, now],
            )?;
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

/// Ищет корень проекта, поднимаясь вверх от начальной директории к ближайшим маркерам (.git, Cargo.toml, package.json и др.).
pub fn find_project_root(start_dir: &Path) -> PathBuf {
    let mut current = if start_dir.is_file() {
        start_dir.parent().unwrap_or(start_dir).to_path_buf()
    } else {
        start_dir.to_path_buf()
    };

    loop {
        if current.join(".git").exists()
            || current.join("Cargo.toml").exists()
            || current.join("package.json").exists()
            || current.join("pyproject.toml").exists()
            || current.join("requirements.txt").exists()
            || current.join("go.mod").exists()
            || current.join("composer.json").exists()
            || current.join("pubspec.yaml").exists()
            || current.join("pom.xml").exists()
            || current.join("build.gradle").exists()
        {
            return current;
        }

        if let Some(parent) = current.parent() {
            if parent == current {
                break;
            }
            current = parent.to_path_buf();
        } else {
            break;
        }
    }

    start_dir.to_path_buf()
}

/// Извлекает название, стек технологий и описание из манифестов репозитория.
pub fn detect_manifest_metadata(root: &Path) -> (String, Vec<String>, Option<String>) {
    let default_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());
    let mut name = default_name.clone();
    let mut tech_stack = Vec::new();
    let mut description = None;

    // 1. Rust: Cargo.toml
    let cargo_toml = root.join("Cargo.toml");
    if cargo_toml.exists() {
        tech_stack.push("rust".to_string());
        if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("name = ") {
                    let n = trimmed.trim_start_matches("name = ").trim().trim_matches('"').trim_matches('\'');
                    if !n.is_empty() {
                        name = n.to_string();
                    }
                } else if trimmed.starts_with("description = ") && description.is_none() {
                    let d = trimmed.trim_start_matches("description = ").trim().trim_matches('"').trim_matches('\'');
                    if !d.is_empty() {
                        description = Some(d.to_string());
                    }
                }
            }
        }
    }

    // 2. Node / TS: package.json
    let package_json = root.join("package.json");
    if package_json.exists() {
        if !tech_stack.contains(&"node".to_string()) {
            tech_stack.push("node".to_string());
        }
        if root.join("tsconfig.json").exists() && !tech_stack.contains(&"typescript".to_string()) {
            tech_stack.push("typescript".to_string());
        }
        if let Ok(content) = std::fs::read_to_string(&package_json) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(n) = v.get("name").and_then(|x| x.as_str()) {
                    if !n.is_empty() && name == default_name {
                        name = n.to_string();
                    }
                }
                if description.is_none() {
                    description = v.get("description").and_then(|x| x.as_str()).map(|s| s.to_string());
                }
                if v.get("dependencies").and_then(|d| d.get("react")).is_some() && !tech_stack.contains(&"react".to_string()) {
                    tech_stack.push("react".to_string());
                }
            }
        }
    }

    // 3. Python: pyproject.toml / requirements.txt
    if root.join("pyproject.toml").exists() || root.join("requirements.txt").exists() || root.join("setup.py").exists() {
        if !tech_stack.contains(&"python".to_string()) {
            tech_stack.push("python".to_string());
        }
        if let Ok(content) = std::fs::read_to_string(root.join("pyproject.toml")) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("name = ") {
                    let n = trimmed.trim_start_matches("name = ").trim().trim_matches('"').trim_matches('\'');
                    if !n.is_empty() && name == default_name {
                        name = n.to_string();
                    }
                }
            }
        }
    }

    // 4. PHP: composer.json
    let composer_json = root.join("composer.json");
    if composer_json.exists() {
        if !tech_stack.contains(&"php".to_string()) {
            tech_stack.push("php".to_string());
        }
        if let Ok(content) = std::fs::read_to_string(&composer_json) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(n) = v.get("name").and_then(|x| x.as_str()) {
                    let short_name = n.split('/').last().unwrap_or(n);
                    if !short_name.is_empty() && name == default_name {
                        name = short_name.to_string();
                    }
                }
            }
        }
    }

    // 5. Go: go.mod
    let go_mod = root.join("go.mod");
    if go_mod.exists() {
        if !tech_stack.contains(&"go".to_string()) {
            tech_stack.push("go".to_string());
        }
        if let Ok(content) = std::fs::read_to_string(&go_mod) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("module ") {
                    let mod_path = trimmed.trim_start_matches("module ").trim();
                    let short_name = mod_path.split('/').last().unwrap_or(mod_path);
                    if !short_name.is_empty() && name == default_name {
                        name = short_name.to_string();
                    }
                    break;
                }
            }
        }
    }

    // 6. Dart/Flutter: pubspec.yaml
    if root.join("pubspec.yaml").exists() && !tech_stack.contains(&"dart".to_string()) {
        tech_stack.push("dart".to_string());
    }

    // 7. Java: pom.xml / build.gradle
    if (root.join("pom.xml").exists() || root.join("build.gradle").exists()) && !tech_stack.contains(&"java".to_string()) {
        tech_stack.push("java".to_string());
    }

    // Если нет описания - проверяем первую текстовую строку в README.md
    if description.is_none() {
        if let Ok(content) = std::fs::read_to_string(root.join("README.md")) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    description = Some(trimmed.chars().take(200).collect());
                    break;
                }
            }
        }
    }

    (name, tech_stack, description)
}
