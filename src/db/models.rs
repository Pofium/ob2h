//! Модели данных SQLite (соответствуют схемам из docs/ARCHITECTURE.md §2).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub description: Option<String>,
    pub tech_stack: Option<String>,
    pub active_branch: Option<String>,
    pub last_scanned_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: i64,
    pub key: String,
    pub content: String,
    pub category: String,
    pub importance: f64,
    pub source: Option<String>,
    pub meta: Option<String>,
    pub embedding: Option<Vec<u8>>,
    pub created_at: String,
    pub updated_at: String,
    pub access_count: i64,
    pub last_accessed: Option<String>,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRelationRecord {
    pub source_key: String,
    pub target_key: String,
    pub relation_type: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRecord {
    pub id: i64,
    pub title: Option<String>,
    pub path: Option<String>,
    pub meta: Option<String>,
    pub created_at: String,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub id: i64,
    pub doc_id: i64,
    pub ordinal: i64,
    pub text: String,
    pub embedding: Option<Vec<u8>>,
    pub created_at: String,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeRecord {
    pub id: i64,
    pub node_id: String,
    pub label: String,
    pub node_type: String,
    pub description: Option<String>,
    pub val: i64,
    pub embedding: Option<Vec<u8>>,
    pub created_at: String,
    pub updated_at: String,
    pub project_id: Option<String>,
    pub file_path: Option<String>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub provenance: Option<String>,
    pub is_god_node: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdgeRecord {
    pub id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub label: String,
    pub weight: f64,
    pub contexts: Option<String>,
    pub created_at: String,
    pub project_id: Option<String>,
    pub provenance: Option<String>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamRunRecord {
    pub id: i64,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub status: Option<String>,
    pub trigger: Option<String>,
    pub phase_log: Option<String>,
    pub stats: Option<String>,
}
