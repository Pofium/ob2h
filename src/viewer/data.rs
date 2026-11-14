//! Read-only доступ к данным для табов viewer (Ф70.2–70.6).
//!
//! Соединение открывается `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=ON`
//! (двойная гарантия «ни одной записи»); миграции НЕ запускаются. Если RO-открытие
//! невозможно (WAL-recovery требует записи в -wal), fallback — RW-соединение с
//! `query_only=ON`: данные всё равно не меняются.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::lifecycle::timeline::{self, SessionEntry};

/// Открывает БД строго для чтения (без миграций, без создания файлов).
pub fn open_readonly(db_path: &Path) -> anyhow::Result<Connection> {
    // WAL без -shm может отказать не при открытии, а при первом чтении —
    // проверяем probe-запросом и откатываемся на RW+query_only.
    let try_ro = || -> anyhow::Result<Connection> {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_millis(2000))?;
        conn.execute_batch("PRAGMA query_only=ON;")?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))?;
        Ok(conn)
    };
    match try_ro() {
        Ok(conn) => Ok(conn),
        Err(_) => {
            // WAL-recovery может требовать запись в служебные файлы — открываем
            // RW, но query_only запрещает любые изменения данных.
            let conn = Connection::open(db_path)?;
            conn.busy_timeout(Duration::from_millis(2000))?;
            conn.execute_batch("PRAGMA query_only=ON;")?;
            Ok(conn)
        }
    }
}

/// Строка памяти для таба «Память».
#[derive(Debug, Clone)]
pub struct MemoryRow {
    pub id: i64,
    pub key: String,
    pub content: String,
    pub category: String,
    pub importance: f64,
    pub source: String,
    pub trust: f64,
    pub project_id: Option<String>,
    pub meta: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub access_count: i64,
}

/// Фильтры поиска памяти (Ф70.2).
pub struct MemoryFilters<'a> {
    pub query: &'a str,
    pub project: Option<&'a str>,
    pub category: Option<&'a str>,
    pub source: Option<&'a str>,
    pub min_trust: f64,
    pub limit: usize,
}

impl Default for MemoryFilters<'_> {
    fn default() -> Self {
        Self {
            query: "",
            project: None,
            category: None,
            source: None,
            min_trust: 0.0,
            limit: 200,
        }
    }
}

/// Поиск памяти: FTS5 trigram (от 3 символов), иначе LIKE; tombstones скрыты.
pub fn search_memories(conn: &Connection, f: &MemoryFilters<'_>) -> rusqlite::Result<Vec<MemoryRow>> {
    let mut sql = String::from(
        "SELECT m.id, m.key, m.content, m.category, m.importance, m.source, m.trust, \
         m.project_id, m.meta, m.created_at, m.updated_at, m.access_count FROM memories m",
    );
    let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let q = f.query.trim();
    let use_fts = q.chars().count() >= 3;
    if use_fts {
        sql.push_str(" JOIN memories_fts fts ON fts.rowid = m.id");
    }
    sql.push_str(" WHERE m.deleted_at IS NULL");
    if !q.is_empty() {
        if use_fts {
            sql.push_str(" AND memories_fts MATCH ?");
            binds.push(Box::new(format!("\"{}\"", q.replace('"', "\"\""))));
        } else {
            sql.push_str(" AND (m.content LIKE ? ESCAPE '\\' OR m.key LIKE ? ESCAPE '\\')");
            let like = format!(
                "%{}%",
                q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
            );
            binds.push(Box::new(like.clone()));
            binds.push(Box::new(like));
        }
    }
    if let Some(p) = f.project {
        sql.push_str(" AND m.project_id = ?");
        binds.push(Box::new(p.to_string()));
    }
    if let Some(c) = f.category {
        sql.push_str(" AND m.category = ?");
        binds.push(Box::new(c.to_string()));
    }
    if let Some(s) = f.source {
        sql.push_str(" AND m.source = ?");
        binds.push(Box::new(s.to_string()));
    }
    if f.min_trust > 0.0 {
        sql.push_str(" AND m.trust >= ?");
        binds.push(Box::new(f.min_trust));
    }
    sql.push_str(" ORDER BY m.importance DESC, m.updated_at DESC LIMIT ?");
    binds.push(Box::new(f.limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
        |r| {
            Ok(MemoryRow {
                id: r.get(0)?,
                key: r.get(1)?,
                content: r.get(2)?,
                category: r.get(3)?,
                importance: r.get(4)?,
                source: r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "manual".into()),
                trust: r.get(6)?,
                project_id: r.get(7)?,
                meta: r.get(8)?,
                created_at: r.get(9)?,
                updated_at: r.get(10)?,
                access_count: r.get(11)?,
            })
        },
    )?;
    rows.collect()
}

/// Связь памяти для деталей (Ф70.2): kind + key соседней записи.
#[derive(Debug, Clone)]
pub struct MemoryLinkRow {
    pub to_id: i64,
    pub to_key: String,
    pub kind: String,
    pub weight: f64,
}

/// Живые связи записи (tombstones рёбер и целей скрыты).
pub fn memory_links(conn: &Connection, id: i64) -> rusqlite::Result<Vec<MemoryLinkRow>> {
    let mut stmt = conn.prepare(
        "SELECT l.to_id, m.key, l.kind, l.weight FROM memory_links l \
         JOIN memories m ON m.id = l.to_id \
         WHERE l.from_id = ?1 AND l.deleted_at IS NULL AND m.deleted_at IS NULL \
         ORDER BY l.weight DESC LIMIT 50",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok(MemoryLinkRow {
            to_id: r.get(0)?,
            to_key: r.get(1)?,
            kind: r.get(2)?,
            weight: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// Уникальные значения колонки memories для фильтров (белый список колонок).
pub fn distinct_memory_values(conn: &Connection, column: &str) -> rusqlite::Result<Vec<String>> {
    let col = match column {
        "category" | "source" | "project_id" => column,
        _ => return Ok(Vec::new()),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT {col} FROM memories WHERE deleted_at IS NULL AND {col} IS NOT NULL AND {col} != '' ORDER BY {col} LIMIT 100"
    ))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Узел графа для таба «Граф» (Ф70.3).
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: i64,
    pub node_id: String,
    pub label: String,
    pub node_type: String,
    pub description: Option<String>,
    pub project_id: Option<String>,
    pub file_path: Option<String>,
    pub line_start: Option<i64>,
    pub degree: usize,
    pub is_god_node: bool,
}

/// Ребро графа (индексы — id узлов из БД).
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub source: i64,
    pub target: i64,
    pub label: String,
    pub weight: f64,
}

/// Срез графа: топ-узлы по степени + рёбра между ними.
#[derive(Debug, Clone, Default)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub total_nodes: usize,
}

/// Кап узлов из `OB2H_VIEWER_GRAPH_MAX_NODES` (дефолт 300, Ф70.3).
pub fn graph_max_nodes() -> usize {
    std::env::var("OB2H_VIEWER_GRAPH_MAX_NODES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(300)
}

/// Загружает граф: топ `max_nodes` узлов по степени, рёбра только между ними.
pub fn load_graph(conn: &Connection, max_nodes: usize) -> rusqlite::Result<GraphData> {
    let mut stmt = conn.prepare(
        "SELECT id, node_id, label, node_type, description, project_id, file_path, line_start, is_god_node \
         FROM graph_nodes WHERE deleted_at IS NULL",
    )?;
    let mut nodes: Vec<GraphNode> = stmt
        .query_map([], |r| {
            Ok(GraphNode {
                id: r.get(0)?,
                node_id: r.get(1)?,
                label: r.get(2)?,
                node_type: r.get(3)?,
                description: r.get(4)?,
                project_id: r.get(5)?,
                file_path: r.get(6)?,
                line_start: r.get(7)?,
                degree: 0,
                is_god_node: r.get::<_, i64>(8)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total_nodes = nodes.len();

    let mut estmt = conn.prepare(
        "SELECT source_id, target_id, label, weight FROM graph_edges WHERE deleted_at IS NULL",
    )?;
    let all_edges: Vec<GraphEdge> = estmt
        .query_map([], |r| {
            Ok(GraphEdge {
                source: r.get(0)?,
                target: r.get(1)?,
                label: r.get(2)?,
                weight: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Степень узла — по всем живым рёбрам (до отсечения капом).
    let mut degree: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for e in &all_edges {
        *degree.entry(e.source).or_default() += 1;
        *degree.entry(e.target).or_default() += 1;
    }
    for n in &mut nodes {
        n.degree = degree.get(&n.id).copied().unwrap_or(0);
    }
    nodes.sort_by(|a, b| b.degree.cmp(&a.degree).then(a.id.cmp(&b.id)));
    nodes.truncate(max_nodes);

    let keep: std::collections::HashSet<i64> = nodes.iter().map(|n| n.id).collect();
    let edges = all_edges
        .into_iter()
        .filter(|e| keep.contains(&e.source) && keep.contains(&e.target))
        .collect();
    Ok(GraphData { nodes, edges, total_nodes })
}

/// Урок для таба «Уроки · Слоты» (Ф70.5, tombstones скрыты).
#[derive(Debug, Clone)]
pub struct LessonRow {
    pub id: String,
    pub content: String,
    pub context: String,
    pub confidence: f64,
    pub reinforcements: i64,
    pub source: String,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub updated_at: String,
}

/// Живые уроки, сортировка по уверенности.
pub fn load_lessons(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<LessonRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, context, confidence, reinforcements, source, project, tags, updated_at \
         FROM lessons WHERE deleted_at IS NULL ORDER BY confidence DESC, updated_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        let tags_raw: String = r.get(7)?;
        Ok(LessonRow {
            id: r.get(0)?,
            content: r.get(1)?,
            context: r.get(2)?,
            confidence: r.get(3)?,
            reinforcements: r.get(4)?,
            source: r.get(5)?,
            project: r.get(6)?,
            tags: serde_json::from_str(&tags_raw).unwrap_or_default(),
            updated_at: r.get(8)?,
        })
    })?;
    rows.collect()
}

/// Слот для таба «Уроки · Слоты» (Ф70.5).
#[derive(Debug, Clone)]
pub struct SlotRow {
    pub label: String,
    pub project: String,
    pub content: String,
    pub size_limit: i64,
    pub description: String,
    pub pinned: bool,
    pub read_only: bool,
    pub updated_at: String,
}

/// Все слоты (project='' = global).
pub fn load_slots(conn: &Connection) -> rusqlite::Result<Vec<SlotRow>> {
    let mut stmt = conn.prepare(
        "SELECT label, project, content, size_limit, description, pinned, read_only, updated_at \
         FROM slots ORDER BY project, label",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SlotRow {
            label: r.get(0)?,
            project: r.get(1)?,
            content: r.get(2)?,
            size_limit: r.get(3)?,
            description: r.get(4)?,
            pinned: r.get::<_, i64>(5)? != 0,
            read_only: r.get::<_, i64>(6)? != 0,
            updated_at: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// Сводка для таба «Статистика» (Ф70.6).
#[derive(Debug, Clone, Default)]
pub struct StatsData {
    pub schema_version: i64,
    pub db_size_bytes: u64,
    pub memories: i64,
    pub memories_deleted: i64,
    pub graph_nodes: i64,
    pub graph_edges: i64,
    pub lessons: i64,
    pub slots: i64,
    pub secrets: i64,
    pub documents: i64,
    pub chunks: i64,
    pub projects: i64,
    pub top_categories: Vec<(String, i64)>,
    pub top_projects: Vec<(String, i64)>,
    /// Только имена секретов — значения не читаются (Ф70.6).
    pub secret_names: Vec<String>,
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

/// Собирает статистику (все запросы — read-only).
pub fn load_stats(conn: &Connection, db_path: &Path) -> rusqlite::Result<StatsData> {
    let schema_version: i64 = conn
        .query_row(
            "SELECT value FROM kv WHERE key = 'schema_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // Размер БД — по файлам (db + wal + shm), без открытия на запись.
    let mut db_size_bytes = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{}", db_path.display(), suffix));
        if let Ok(md) = std::fs::metadata(&p) {
            db_size_bytes += md.len();
        }
    }

    let top = |sql: &str| -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    };

    let mut secret_names = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT name FROM secrets ORDER BY name LIMIT 200") {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for r in rows.flatten() {
                secret_names.push(r);
            }
        }
    }

    Ok(StatsData {
        schema_version,
        db_size_bytes,
        memories: count(conn, "SELECT count(*) FROM memories WHERE deleted_at IS NULL"),
        memories_deleted: count(conn, "SELECT count(*) FROM memories WHERE deleted_at IS NOT NULL"),
        graph_nodes: count(conn, "SELECT count(*) FROM graph_nodes WHERE deleted_at IS NULL"),
        graph_edges: count(conn, "SELECT count(*) FROM graph_edges WHERE deleted_at IS NULL"),
        lessons: count(conn, "SELECT count(*) FROM lessons WHERE deleted_at IS NULL"),
        slots: count(conn, "SELECT count(*) FROM slots"),
        secrets: count(conn, "SELECT count(*) FROM secrets"),
        documents: count(conn, "SELECT count(*) FROM documents"),
        chunks: count(conn, "SELECT count(*) FROM chunks"),
        projects: count(conn, "SELECT count(*) FROM projects"),
        top_categories: top(
            "SELECT category, count(*) c FROM memories WHERE deleted_at IS NULL \
             GROUP BY category ORDER BY c DESC LIMIT 8",
        )?,
        top_projects: top(
            "SELECT coalesce(project_id, '—'), count(*) c FROM memories WHERE deleted_at IS NULL \
             GROUP BY project_id ORDER BY c DESC LIMIT 8",
        )?,
        secret_names,
    })
}

/// Сессии из daily-логов (Ф70.4): без создания каталогов, битые строки пропускаются.
pub fn load_sessions(data_dir: &Path) -> Vec<SessionEntry> {
    let daily = data_dir.join("workspace").join("daily");
    if !daily.is_dir() {
        return Vec::new();
    }
    timeline::load_entries_from_dir(&daily).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn seed() -> (tempfile::TempDir, Database) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::new(tmp.path().join("ob2h.db")).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memories (key, content, category, importance, source, trust, created_at, updated_at) \
                 VALUES ('k1', 'rust viewer test', 'general', 0.8, 'manual', 0.6, '2026-01-01', '2026-01-01')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        (tmp, db)
    }

    #[test]
    fn readonly_connection_rejects_writes() {
        let (tmp, db) = seed();
        db.with_conn(|conn| conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);"))
            .unwrap();
        let ro = open_readonly(&tmp.path().join("ob2h.db")).unwrap();
        let err = ro.execute("INSERT INTO kv (key, value) VALUES ('x', 'y')", []);
        assert!(err.is_err(), "query_only должен запрещать запись");
    }

    #[test]
    fn search_filters_and_fts() {
        let (tmp, _db) = seed();
        let ro = open_readonly(&tmp.path().join("ob2h.db")).unwrap();
        let f = MemoryFilters { query: "viewer", ..Default::default() };
        assert_eq!(search_memories(&ro, &f).unwrap().len(), 1);
        let f = MemoryFilters { query: "ew", ..Default::default() }; // короткий — LIKE
        assert_eq!(search_memories(&ro, &f).unwrap().len(), 1);
        let f = MemoryFilters { min_trust: 0.9, ..Default::default() };
        assert_eq!(search_memories(&ro, &f).unwrap().len(), 0);
        let f = MemoryFilters { category: Some("general"), ..Default::default() };
        assert_eq!(search_memories(&ro, &f).unwrap().len(), 1);
    }

    #[test]
    fn graph_cap_keeps_top_degree() {
        let (tmp, db) = seed();
        db.with_conn(|conn| {
            for i in 0..8 {
                conn.execute(
                    "INSERT INTO graph_nodes (node_id, label, node_type, created_at, updated_at) \
                     VALUES (?1, ?2, 'Concept', '2026-01-01', '2026-01-01')",
                    rusqlite::params![format!("n{i}"), format!("N{i}")],
                )?;
            }
            // Узел 1 — хаб: рёбра ко всем остальным.
            for i in 2..=8 {
                conn.execute(
                    "INSERT INTO graph_edges (source_id, target_id, label, created_at) \
                     VALUES (1, ?1, 'RELATED', '2026-01-01')",
                    [i],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let ro = open_readonly(&tmp.path().join("ob2h.db")).unwrap();
        let g = load_graph(&ro, 3).unwrap();
        assert_eq!(g.total_nodes, 8);
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.nodes[0].id, 1, "хаб должен быть первым");
        assert!(g.edges.iter().all(|e| e.source == 1));
    }

    #[test]
    fn stats_counts_and_secret_names_only() {
        let (tmp, _db) = seed();
        let ro = open_readonly(&tmp.path().join("ob2h.db")).unwrap();
        let s = load_stats(&ro, &tmp.path().join("ob2h.db")).unwrap();
        assert_eq!(s.memories, 1);
        assert_eq!(s.schema_version, crate::db::schema::SCHEMA_VERSION);
        assert!(s.db_size_bytes > 0);
        assert_eq!(s.top_categories[0].0, "general");
    }

    #[test]
    fn sessions_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_sessions(tmp.path()).is_empty());
    }
}