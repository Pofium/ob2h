//! Схема SQLite и версионные миграции (ADR-1…ADR-3).

use rusqlite::{params, Connection, Result};

pub const SCHEMA_VERSION: i64 = 4;

/// M2 (v0.9+): столбцы синхронизации. origin='' означает «создано/изменено этим
/// узлом» (при экспорте нормализуется в origin из peers.json); deleted_at —
/// tombstone (LWW-совместимое удаление); updated_at рёбер — для watermark/LWW.
pub const MIGRATION_V2: &str = r#"
ALTER TABLE memories ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE memories ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_nodes ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE graph_nodes ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_edges ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE graph_edges ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_edges ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';
UPDATE graph_edges SET updated_at = created_at WHERE updated_at = '';

CREATE TABLE IF NOT EXISTS sync_state (
  peer TEXT PRIMARY KEY,
  last_export_at TEXT,
  last_import_at TEXT,
  applied_bundles TEXT NOT NULL DEFAULT '[]'
);
"#;

/// M3 (v1.0+): пространства проектов, AST-детерминированный граф,
/// маркировка достоверности связей (provenance/confidence) и узлы-боги (God Nodes).
pub const MIGRATION_V3: &str = r#"
CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  root_path TEXT NOT NULL,
  description TEXT,
  tech_stack TEXT,
  active_branch TEXT,
  last_scanned_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_projects_root_path ON projects(root_path);

ALTER TABLE memories ADD COLUMN project_id TEXT;
CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);

ALTER TABLE documents ADD COLUMN project_id TEXT;
CREATE INDEX IF NOT EXISTS idx_documents_project ON documents(project_id);

ALTER TABLE chunks ADD COLUMN project_id TEXT;
CREATE INDEX IF NOT EXISTS idx_chunks_project ON chunks(project_id);

ALTER TABLE graph_nodes ADD COLUMN project_id TEXT;
ALTER TABLE graph_nodes ADD COLUMN file_path TEXT;
ALTER TABLE graph_nodes ADD COLUMN line_start INTEGER;
ALTER TABLE graph_nodes ADD COLUMN line_end INTEGER;
ALTER TABLE graph_nodes ADD COLUMN provenance TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE graph_nodes ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0;
ALTER TABLE graph_nodes ADD COLUMN is_god_node INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_graph_nodes_project ON graph_nodes(project_id);
CREATE INDEX IF NOT EXISTS idx_graph_nodes_provenance ON graph_nodes(provenance);

ALTER TABLE graph_edges ADD COLUMN project_id TEXT;
ALTER TABLE graph_edges ADD COLUMN provenance TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE graph_edges ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0;
CREATE INDEX IF NOT EXISTS idx_graph_edges_project ON graph_edges(project_id);
"#;

/// M4 (v1.2+): project_files для честного инкрементального AST-сканирования.
pub const MIGRATION_V4: &str = r#"
CREATE TABLE IF NOT EXISTS project_files (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  rel_path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  file_size INTEGER NOT NULL,
  lines_count INTEGER NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (project_id, rel_path)
);
CREATE INDEX IF NOT EXISTS idx_project_files_project ON project_files(project_id);
"#;

/// Текущая версия схемы БД (0 — свежая, ещё без таблиц).
pub fn schema_version(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT value FROM kv WHERE key = 'schema_version'",
        [],
        |row| {
            let val: String = row.get(0)?;
            Ok(val.parse::<i64>().unwrap_or(0))
        },
    )
    .unwrap_or(0)
}

pub const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS memories (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  key TEXT UNIQUE NOT NULL,
  content TEXT NOT NULL,
  category TEXT NOT NULL DEFAULT 'general',
  importance REAL NOT NULL DEFAULT 0.5,
  source TEXT DEFAULT 'manual',
  meta TEXT,
  embedding BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  access_count INTEGER NOT NULL DEFAULT 0,
  last_accessed TEXT
);
CREATE INDEX IF NOT EXISTS idx_memories_cat_imp
  ON memories (category, importance DESC);

CREATE TABLE IF NOT EXISTS memory_relations (
  source_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
  target_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
  relation_type TEXT NOT NULL,
  weight REAL NOT NULL DEFAULT 1.0,
  UNIQUE (source_key, target_key, relation_type)
);

CREATE TABLE IF NOT EXISTS documents (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  title TEXT,
  path TEXT,
  meta TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  text TEXT NOT NULL,
  embedding BLOB,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chunks_doc ON chunks (doc_id);

CREATE TABLE IF NOT EXISTS graph_nodes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  node_id TEXT UNIQUE NOT NULL,
  label TEXT NOT NULL,
  node_type TEXT NOT NULL,
  description TEXT,
  val INTEGER NOT NULL DEFAULT 1,
  embedding BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_graph_nodes_label ON graph_nodes (label);
CREATE INDEX IF NOT EXISTS idx_graph_nodes_type ON graph_nodes (node_type);

CREATE TABLE IF NOT EXISTS graph_edges (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
  target_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  weight REAL NOT NULL DEFAULT 1.0,
  contexts TEXT,
  created_at TEXT NOT NULL,
  UNIQUE (source_id, target_id, label)
);
CREATE INDEX IF NOT EXISTS idx_graph_edges_src ON graph_edges (source_id);
CREATE INDEX IF NOT EXISTS idx_graph_edges_dst ON graph_edges (target_id);

CREATE TABLE IF NOT EXISTS dream_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at TEXT,
  finished_at TEXT,
  status TEXT,
  trigger TEXT,
  phase_log TEXT,
  stats TEXT
);

CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
  content, content='memories', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER IF NOT EXISTS memories_fts_ai AFTER INSERT ON memories BEGIN
  INSERT INTO memories_fts (rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS memories_fts_ad AFTER DELETE ON memories BEGIN
  INSERT INTO memories_fts (memories_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER IF NOT EXISTS memories_fts_au AFTER UPDATE ON memories BEGIN
  INSERT INTO memories_fts (memories_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
  INSERT INTO memories_fts (rowid, content) VALUES (new.id, new.content);
END;

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  text, content='chunks', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts (rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts (chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE ON chunks BEGIN
  INSERT INTO chunks_fts (chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
  INSERT INTO chunks_fts (rowid, text) VALUES (new.id, new.text);
END;
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;"
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        [],
    )?;

    let current_version: i64 = schema_version(conn);

    if current_version < 1 {
        // Свежая БД: все миграции по порядку.
        conn.execute_batch(MIGRATION_V1)?;
        conn.execute_batch(MIGRATION_V2)?;
        conn.execute_batch(MIGRATION_V3)?;
        conn.execute_batch(MIGRATION_V4)?;
    } else {
        if current_version < 2 {
            conn.execute_batch(MIGRATION_V2)?;
        }
        if current_version < 3 {
            conn.execute_batch(MIGRATION_V3)?;
        }
        if current_version < 4 {
            conn.execute_batch(MIGRATION_V4)?;
        }
    }

    if current_version < SCHEMA_VERSION {
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }

    Ok(())
}
