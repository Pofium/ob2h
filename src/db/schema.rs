//! Схема SQLite и версионные миграции (ADR-1…ADR-3).

use rusqlite::{params, Connection, Result};

pub const SCHEMA_VERSION: i64 = 13;

/// M13 (v2.0, Ф73–Ф74): телеметрия U-PPR (`access_count`, `trust_score`, `status`
/// для `SUPERSEDED`/`ACTIVE` на `graph_nodes`), временны́е границы `valid_until` и
/// `git_head_sha` (git-anchored facts) на `memories`.
pub const MIGRATION_V13: &str = r#"
ALTER TABLE graph_nodes ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE graph_nodes ADD COLUMN trust_score REAL NOT NULL DEFAULT 1.0;
ALTER TABLE graph_nodes ADD COLUMN status TEXT NOT NULL DEFAULT 'ACTIVE';
ALTER TABLE memories ADD COLUMN valid_until TEXT;
ALTER TABLE memories ADD COLUMN git_head_sha TEXT;
ALTER TABLE memories ADD COLUMN status TEXT NOT NULL DEFAULT 'ACTIVE';
CREATE INDEX IF NOT EXISTS idx_graph_nodes_status ON graph_nodes(status);
CREATE INDEX IF NOT EXISTS idx_memories_status ON memories(status);
"#;

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

/// M5 (v1.3, Фаза 23): trust и связи памяти. trust — машинный сигнал подтверждённости
/// (использование/ревизия), отделён от авторского importance; memory_links —
/// детерминированные автосвязи (kind: same_project|entity|category|manual;
/// contradicts|causes|supersedes — резерв под dream-ревизию, PLAN_v1.3 §23.5).
pub const MIGRATION_V5: &str = r#"
ALTER TABLE memories ADD COLUMN trust REAL NOT NULL DEFAULT 0.5;
ALTER TABLE memories ADD COLUMN last_feedback_at TEXT;
CREATE INDEX IF NOT EXISTS idx_memories_trust ON memories(trust);

CREATE TABLE IF NOT EXISTS memory_links (
  from_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  to_id   INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  kind    TEXT NOT NULL,
  weight  REAL NOT NULL DEFAULT 1.0,
  created_at TEXT NOT NULL,
  PRIMARY KEY (from_id, to_id, kind)
);
CREATE INDEX IF NOT EXISTS idx_memlink_from ON memory_links(from_id);
CREATE INDEX IF NOT EXISTS idx_memlink_to   ON memory_links(to_id);
"#;

/// M7 (v1.4, Фаза 32): soft-delete рёбер памяти — забытая сторона ребра получает
/// tombstone (deleted_at), ребро реплицируется синком v2 (Ф34), как tombstones записей.
pub const MIGRATION_V7: &str = r#"
ALTER TABLE memory_links ADD COLUMN deleted_at TEXT;
CREATE INDEX IF NOT EXISTS idx_memlink_deleted ON memory_links(deleted_at);
"#;

/// M8 (v1.8+): ast_refs для точного инкрементального перерезолва межфайловых CALLS/IMPORTS.
pub const MIGRATION_V8: &str = r#"
CREATE TABLE IF NOT EXISTS ast_refs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  rel_path TEXT NOT NULL,
  line INTEGER NOT NULL,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  module TEXT,
  item TEXT,
  alias TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ast_refs_proj_path ON ast_refs(project_id, rel_path);
CREATE INDEX IF NOT EXISTS idx_ast_refs_target_name ON ast_refs(project_id, name);
"#;

/// M6 (v1.3, Фазы 26–27): Ralph Knowledge Layer — циклы разработки.
/// Аддитивно; graph_nodes НЕ пересоздаётся (ручная колонка confidence — спека §4.1).
pub const MIGRATION_V6: &str = r#"
CREATE TABLE IF NOT EXISTS ralph_runs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  feature_slug TEXT NOT NULL,
  goal TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'proposed',
  autonomy TEXT NOT NULL DEFAULT 'L1',
  max_iterations_per_task INTEGER NOT NULL DEFAULT 5,
  max_total_iterations INTEGER NOT NULL DEFAULT 60,
  budget_tokens INTEGER,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  finished_at TEXT,
  stop_reason TEXT,
  UNIQUE (project_id, feature_slug)
);

CREATE TABLE IF NOT EXISTS ralph_iterations (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES ralph_runs(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL,
  n INTEGER NOT NULL,
  git_before TEXT,
  git_after TEXT,
  hypothesis TEXT,
  plan TEXT,
  result TEXT,
  tests_summary TEXT,
  verdict TEXT NOT NULL DEFAULT 'unconfirmed'
    CHECK (verdict IN ('verified','failed','unconfirmed','overturned','stale')),
  verdict_source TEXT,
  ladder_rung TEXT,
  context_ref TEXT,
  tokens_used INTEGER,
  duration_ms INTEGER,
  created_at TEXT NOT NULL,
  UNIQUE (run_id, task_id, n)
);
CREATE INDEX IF NOT EXISTS idx_ralph_iterations_run ON ralph_iterations(run_id, task_id, n);

CREATE TABLE IF NOT EXISTS ralph_findings (
  id TEXT PRIMARY KEY,
  run_id TEXT REFERENCES ralph_runs(id) ON DELETE SET NULL,
  iteration_id TEXT REFERENCES ralph_iterations(id) ON DELETE SET NULL,
  project_id TEXT REFERENCES projects(id),
  kind TEXT NOT NULL,
  content TEXT NOT NULL,
  symbols TEXT NOT NULL DEFAULT '[]',
  verdict TEXT NOT NULL DEFAULT 'unconfirmed'
    CHECK (verdict IN ('verified','failed','unconfirmed','overturned','stale')),
  verdict_source TEXT,
  embedding BLOB,
  meta TEXT,
  created_at TEXT NOT NULL,
  stale_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_ralph_findings_proj ON ralph_findings(project_id, verdict);

CREATE TABLE IF NOT EXISTS ast_changes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  run_id TEXT REFERENCES ralph_runs(id) ON DELETE SET NULL,
  iteration_id TEXT REFERENCES ralph_iterations(id) ON DELETE SET NULL,
  path TEXT NOT NULL,
  node_key TEXT NOT NULL,
  label TEXT NOT NULL,
  node_type TEXT NOT NULL,
  change_type TEXT NOT NULL,
  sig_before TEXT,
  sig_after TEXT,
  loc_delta INTEGER DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ast_changes_proj ON ast_changes(project_id, path);
CREATE INDEX IF NOT EXISTS idx_ast_changes_iter ON ast_changes(iteration_id);
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


pub const MIGRATION_V9: &str = r#"
-- Хранилище зашифрованных секретов (v1.9, Фаза 56)
CREATE TABLE IF NOT EXISTS secrets (
    name TEXT PRIMARY KEY,
    ciphertext BLOB NOT NULL,
    hint TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_accessed TEXT,
    access_count INTEGER NOT NULL DEFAULT 0
);
"#;

/// M10 (v1.9, Фаза 62.1): FTS5-индекс `graph_nodes(label, description)` для
/// быстрого лексического поиска графа (BM25) вместо Full Scan `LIKE %query%`.
/// Внешний контент (content='graph_nodes') — дублирования данных нет; триггеры
/// синхронизируют индекс с таблицей; backfill проставляет существующие узлы.
pub const MIGRATION_V10: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS graph_nodes_fts USING fts5(
  label, description, content='graph_nodes', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER IF NOT EXISTS graph_nodes_fts_ai AFTER INSERT ON graph_nodes BEGIN
  INSERT INTO graph_nodes_fts (rowid, label, description)
    VALUES (new.id, new.label, new.description);
END;
CREATE TRIGGER IF NOT EXISTS graph_nodes_fts_ad AFTER DELETE ON graph_nodes BEGIN
  INSERT INTO graph_nodes_fts (graph_nodes_fts, rowid, label, description)
    VALUES ('delete', old.id, old.label, old.description);
END;
CREATE TRIGGER IF NOT EXISTS graph_nodes_fts_au AFTER UPDATE ON graph_nodes BEGIN
  INSERT INTO graph_nodes_fts (graph_nodes_fts, rowid, label, description)
    VALUES ('delete', old.id, old.label, old.description);
  INSERT INTO graph_nodes_fts (rowid, label, description)
    VALUES (new.id, new.label, new.description);
END;
INSERT INTO graph_nodes_fts (rowid, label, description)
  SELECT id, label, description FROM graph_nodes;
"#;

/// M11 (v1.9.9, Фаза 64): жизненный цикл памяти — уроки (lessons) с FTS5-индексом,
/// слоты рабочего контекста (slots) и аудит-леджер (audit_log). Только CREATE
/// (без изменения существующих таблиц), по образцу M9/M10.
/// Отступления от эскиза PLAN_v1.9.9 §5.1: у lessons PK — `seq INTEGER` (rowid-алиас,
/// нужен для content_rowid FTS5 external content) + `id TEXT UNIQUE` как дедуп-ключ;
/// у slots PK — `(label, project)` (project='' = global), чтобы одноимённые слоты
/// разных проектов не коллидировали; у обеих таблиц есть origin — LWW-мердж синка
/// (Ф65.4) работает по тем же правилам, что у memories.
pub const MIGRATION_V11: &str = r#"
CREATE TABLE IF NOT EXISTS lessons (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    content TEXT NOT NULL,
    context TEXT NOT NULL DEFAULT '',
    confidence REAL NOT NULL DEFAULT 0.5,
    reinforcements INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'manual',
    source_ids TEXT NOT NULL DEFAULT '[]',
    project TEXT,
    tags TEXT NOT NULL DEFAULT '[]',
    decay_rate REAL NOT NULL DEFAULT 0.05,
    origin TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_reinforced_at TEXT,
    last_decayed_at TEXT,
    deleted_at TEXT
);
CREATE INDEX IF NOT EXISTS lessons_project_idx ON lessons(project, deleted_at);

CREATE VIRTUAL TABLE IF NOT EXISTS lessons_fts USING fts5(
  content, context, content='lessons', content_rowid='seq', tokenize='trigram'
);
CREATE TRIGGER IF NOT EXISTS lessons_fts_ai AFTER INSERT ON lessons BEGIN
  INSERT INTO lessons_fts (rowid, content, context)
    VALUES (new.seq, new.content, new.context);
END;
CREATE TRIGGER IF NOT EXISTS lessons_fts_ad AFTER DELETE ON lessons BEGIN
  INSERT INTO lessons_fts (lessons_fts, rowid, content, context)
    VALUES ('delete', old.seq, old.content, old.context);
END;
CREATE TRIGGER IF NOT EXISTS lessons_fts_au AFTER UPDATE ON lessons BEGIN
  INSERT INTO lessons_fts (lessons_fts, rowid, content, context)
    VALUES ('delete', old.seq, old.content, old.context);
  INSERT INTO lessons_fts (rowid, content, context)
    VALUES (new.seq, new.content, new.context);
END;
INSERT INTO lessons_fts (rowid, content, context)
  SELECT seq, content, context FROM lessons;

CREATE TABLE IF NOT EXISTS slots (
    label TEXT NOT NULL,
    project TEXT NOT NULL DEFAULT '',
    content TEXT NOT NULL DEFAULT '',
    size_limit INTEGER NOT NULL DEFAULT 2000,
    description TEXT NOT NULL DEFAULT '',
    pinned INTEGER NOT NULL DEFAULT 1,
    read_only INTEGER NOT NULL DEFAULT 0,
    origin TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (label, project)
);

CREATE TABLE IF NOT EXISTS audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target_ids TEXT NOT NULL DEFAULT '[]',
    details TEXT NOT NULL DEFAULT '{}',
    source_tool TEXT
);
CREATE INDEX IF NOT EXISTS audit_log_action_idx ON audit_log(action, id);
"#;

/// M12 (v2.0, Фазы 71.3/72b.2/72b.5/73.9 — вся схема v12 в одной транзакции):
/// (1) словарь provenance v2.0 — EXTRACTED/ast → SYNTAX, INFERRED → HEURISTIC
///     (аддитивные GIT_HISTORY/manual не трогаются); (2) `graph_nodes.kind_detail`
///     (тестовые узлы 71.2 и прочие detail); (3) `graph_edges.confidence` — колонка
///     УЖЕ есть с M3 (REAL NOT NULL DEFAULT 1.0), эвристики 71.x ставят <1.0;
///     (4) `graph_nodes.subtokens` — саб-токеновая
///     декомпозиция идентификаторов для FTS5 (73.9, Zulip/GitHub-стиль);
///     (5) `bq_mask BLOB` — битовые маски бинарного квантования рядом с
///     вектором (72b.2, mean-centered BQ, 48/96 байт); (6) `file_blobs` —
///     content-addressed кэш парсинга/векторизации (72b.5, blake3).
///     (7) перестройка graph_nodes_fts с колонкой subtokens (external content —
///     колонки маппятся по имени), заполнение подтокенов — Rust-проход после SQL.
pub const MIGRATION_V12: &str = r#"
BEGIN;
UPDATE graph_edges SET provenance='SYNTAX' WHERE provenance IN ('EXTRACTED', 'ast');
UPDATE graph_edges SET provenance='HEURISTIC' WHERE provenance='INFERRED';
UPDATE graph_nodes SET provenance='SYNTAX' WHERE provenance IN ('EXTRACTED', 'ast');
UPDATE graph_nodes SET provenance='HEURISTIC' WHERE provenance='INFERRED';
ALTER TABLE graph_nodes ADD COLUMN kind_detail TEXT;
ALTER TABLE graph_nodes ADD COLUMN subtokens TEXT NOT NULL DEFAULT '';
ALTER TABLE graph_nodes ADD COLUMN bq_mask BLOB;
ALTER TABLE memories ADD COLUMN bq_mask BLOB;
CREATE TABLE IF NOT EXISTS file_blobs (
    blake3_hash TEXT PRIMARY KEY,
    rel_path TEXT NOT NULL,
    project_id TEXT,
    parser_version TEXT NOT NULL DEFAULT '',
    embedding_fingerprint TEXT NOT NULL DEFAULT '',
    ast_json TEXT NOT NULL DEFAULT '',
    vector_ids TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
DROP TRIGGER IF EXISTS graph_nodes_fts_ai;
DROP TRIGGER IF EXISTS graph_nodes_fts_ad;
DROP TRIGGER IF EXISTS graph_nodes_fts_au;
DROP TABLE IF EXISTS graph_nodes_fts;
CREATE VIRTUAL TABLE graph_nodes_fts USING fts5(
  label, description, subtokens, content='graph_nodes', content_rowid='id', tokenize='trigram'
);
COMMIT;
"#;

/// M12, шаг 3 (после Rust-прохода подтокенов): официальный full rebuild
/// external-content FTS (индекс согласован по построению) + возврат триггеров.
/// Порядок критичен: 'delete'-команды триггеров по пустому индексу коррумпируют
/// external content — поэтому триггеры создаются только после rebuild.
pub const MIGRATION_V12_FTS_REBUILD: &str = r#"
INSERT INTO graph_nodes_fts (graph_nodes_fts) VALUES ('rebuild');
CREATE TRIGGER graph_nodes_fts_ai AFTER INSERT ON graph_nodes BEGIN
  INSERT INTO graph_nodes_fts (rowid, label, description, subtokens)
    VALUES (new.id, new.label, new.description, new.subtokens);
END;
CREATE TRIGGER graph_nodes_fts_ad AFTER DELETE ON graph_nodes BEGIN
  INSERT INTO graph_nodes_fts (graph_nodes_fts, rowid, label, description, subtokens)
    VALUES ('delete', old.id, old.label, old.description, old.subtokens);
END;
CREATE TRIGGER graph_nodes_fts_au AFTER UPDATE ON graph_nodes BEGIN
  INSERT INTO graph_nodes_fts (graph_nodes_fts, rowid, label, description, subtokens)
    VALUES ('delete', old.id, old.label, old.description, old.subtokens);
  INSERT INTO graph_nodes_fts (rowid, label, description, subtokens)
    VALUES (new.id, new.label, new.description, new.subtokens);
END;
"#;

/// Ф73.9: саб-токеновая декомпозиция идентификатора (snake_case/camelCase/
/// kebab/точки) — колонка `graph_nodes.subtokens` и декомпозиция запросов
/// (поиск «worker pool» находит `EmbeddingWorkerPool` без смены токенизатора).
pub fn decompose_identifier(s: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for chunk in s.split(['_', '-', '.', ' ']) {
        if chunk.is_empty() {
            continue;
        }
        let chars: Vec<char> = chunk.chars().collect();
        let mut cur = String::new();
        for (i, ch) in chars.iter().enumerate() {
            // Границы: (1) upper после lower/digit — «workerPool»;
            // (2) lower после акронима («HTTPServer») — последний upper акронима
            // остаётся в новой части («s» → «server»).
            let boundary_kind = if i == 0 {
                None
            } else {
                let prev = chars[i - 1];
                if ch.is_ascii_uppercase() && (prev.is_ascii_lowercase() || prev.is_ascii_digit())
                {
                    Some(false) // смена части целиком
                } else if ch.is_ascii_lowercase()
                    && prev.is_ascii_uppercase()
                    && i >= 2
                    && chars[i - 2].is_ascii_uppercase()
                {
                    Some(true) // хвост акронима переносится в новую часть
                } else {
                    None
                }
            };
            if let Some(keep_last) = boundary_kind {
                if !cur.is_empty() {
                    let head = if keep_last && cur.chars().count() > 1 {
                        cur.chars().take(cur.chars().count() - 1).collect()
                    } else {
                        cur.clone()
                    };
                    if !head.is_empty() {
                        parts.push(head.to_lowercase());
                    }
                    cur = String::new();
                    if keep_last {
                        cur.push(chars[i - 1]); // «S» уходит в «server»
                    }
                }
            }
            cur.push(*ch);
        }
        if !cur.is_empty() {
            parts.push(cur.to_lowercase());
        }
    }
    parts.join(" ")
}

/// M12: заполнить `graph_nodes.subtokens` декомпозицией label для всех узлов
/// (один раз при миграции; новые узлы заполняются при вставке — 73.9).
fn populate_fts_subtokens(conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, label FROM graph_nodes")?;
        let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        it.collect::<std::result::Result<_, _>>()?
    };
    for (id, label) in rows {
        let sub = decompose_identifier(&label);
        conn.execute(
            "UPDATE graph_nodes SET subtokens = ?1 WHERE id = ?2",
            params![sub, id],
        )?;
    }
    Ok(())
}

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
        conn.execute_batch(MIGRATION_V5)?;
        conn.execute_batch(MIGRATION_V6)?;
        conn.execute_batch(MIGRATION_V7)?;
        conn.execute_batch(MIGRATION_V8)?;
        conn.execute_batch(MIGRATION_V9)?;
        conn.execute_batch(MIGRATION_V10)?;
        conn.execute_batch(MIGRATION_V11)?;
        conn.execute_batch(MIGRATION_V12)?;
        populate_fts_subtokens(conn)?;
        conn.execute_batch(MIGRATION_V12_FTS_REBUILD)?;
        conn.execute_batch(MIGRATION_V13)?;
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
        if current_version < 5 {
            conn.execute_batch(MIGRATION_V5)?;
        }
        if current_version < 6 {
            conn.execute_batch(MIGRATION_V6)?;
        }
        if current_version < 7 {
            conn.execute_batch(MIGRATION_V7)?;
        }
        if current_version < 8 {
            conn.execute_batch(MIGRATION_V8)?;
        }
        if current_version < 9 {
            conn.execute_batch(MIGRATION_V9)?;
        }
        if current_version < 10 {
            conn.execute_batch(MIGRATION_V10)?;
        }
        if current_version < 11 {
            conn.execute_batch(MIGRATION_V11)?;
        }
        if current_version < 12 {
            conn.execute_batch(MIGRATION_V12)?;
            populate_fts_subtokens(conn)?;
            conn.execute_batch(MIGRATION_V12_FTS_REBUILD)?;
        }
        if current_version < 13 {
            conn.execute_batch(MIGRATION_V13)?;
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

#[cfg(test)]
mod subtoken_tests {
    use super::decompose_identifier;

    #[test]
    fn decompose_snake_camel_kebab() {
        assert_eq!(decompose_identifier("parse_source_file"), "parse source file");
        assert_eq!(decompose_identifier("EmbeddingWorkerPool"), "embedding worker pool");
        assert_eq!(decompose_identifier("HTTPServer"), "http server");
        assert_eq!(decompose_identifier("FTS5Table"), "fts5 table");
        assert_eq!(decompose_identifier("user-profile-url"), "user profile url");
        assert_eq!(decompose_identifier("ob2h.db-wal"), "ob2h db wal");
        assert_eq!(decompose_identifier("lower"), "lower");
        assert_eq!(decompose_identifier("x"), "x");
        assert_eq!(decompose_identifier(""), "");
    }
}
