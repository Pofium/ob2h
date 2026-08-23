//! Сервис графа знаний: дедупликация, поиск и KAG-рассуждение.

use std::collections::HashMap;
use std::sync::Arc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::db::{utcnow, Database};
use crate::embedding::EmbeddingProvider;
use crate::extractor::ExtractionResult;
use crate::llm::{LLMClient, LLMClientExt};
use crate::vector::{serialize, top_k};

pub const REASON_SYSTEM_PROMPT: &str = "\
Ты отвечаешь на вопрос по графу знаний личного агента. Опирайся ТОЛЬКО на \
переданные факты. Верни СТРОГО JSON без markdown:
{\"answer\": \"ответ по-русски\",
 \"confidence\": 0.0,
 \"reasoning_steps\": [\"шаг 1\", \"шаг 2\"],
 \"used_entities\": [\"label сущностей\"],
 \"used_relations\": [\"label отношений\"]}
Если фактов недостаточно — так и скажи в answer, confidence = 0.1.";

pub fn make_node_id(label: &str, node_type: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{label}|{node_type}").as_bytes());
    let hash = hex::encode(hasher.finalize());
    hash[..24].to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeWithNeighbors {
    pub id: i64,
    pub node_id: String,
    pub label: String,
    pub node_type: String,
    pub description: Option<String>,
    pub val: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeWithLabels {
    pub id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub source_label: String,
    pub target_label: String,
    pub label: String,
    pub weight: f64,
    pub contexts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphSearchResult {
    pub nodes: Vec<NodeWithNeighbors>,
    pub edges: Vec<EdgeWithLabels>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphReasonResult {
    pub answer: String,
    pub confidence: f64,
    #[serde(default)]
    pub reasoning_steps: Vec<String>,
    #[serde(default)]
    pub used_entities: Vec<String>,
    #[serde(default)]
    pub used_relations: Vec<String>,
    #[serde(default)]
    pub nodes_used: usize,
    #[serde(default)]
    pub edges_used: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub nodes: i64,
    pub edges: i64,
    pub documents: i64,
    pub chunks: i64,
}

pub struct GraphService {
    db: Database,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl GraphService {
    pub fn new(db: Database, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { db, embedder }
    }

    pub async fn upsert_extraction(&self, result: &ExtractionResult) -> anyhow::Result<(usize, usize, usize)> {
        let now = utcnow();
        let mut label_to_rowid: HashMap<String, i64> = HashMap::new();
        let mut new_entities = 0;
        let mut updated_entities = 0;
        let mut new_edges = 0;

        for entity in &result.entities {
            let node_id = make_node_id(&entity.label, &entity.entity_type);
            let existing: Option<(i64, Option<String>)> = self.db.with_conn(|conn| {
                let mut stmt = conn.prepare("SELECT id, description FROM graph_nodes WHERE node_id = ?1")?;
                let mut rows = stmt.query(params![node_id])?;
                if let Some(row) = rows.next()? {
                    Ok(Some((row.get(0)?, row.get(1)?)))
                } else {
                    Ok(None)
                }
            })?;

            if let Some((id, existing_desc)) = existing {
                let mut desc = existing_desc.unwrap_or_default();
                if !entity.description.is_empty() && !desc.contains(&entity.description) {
                    desc = format!("{desc} {}", entity.description).trim().chars().take(2000).collect();
                }

                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE graph_nodes SET val = val + 1, description = ?1, updated_at = ?2, origin = '' WHERE id = ?3",
                        params![desc, now, id],
                    )?;
                    Ok(())
                })?;

                updated_entities += 1;
                label_to_rowid.insert(entity.label.clone(), id);
                self.embed_node(id, &format!("{}: {}", entity.label, desc)).await;
            } else {
                let id: i64 = self.db.with_conn(|conn| {
                    conn.execute(
                        "INSERT INTO graph_nodes (node_id, label, node_type, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                        params![node_id, entity.label, entity.entity_type, entity.description, now],
                    )?;
                    Ok(conn.last_insert_rowid())
                })?;

                new_entities += 1;
                label_to_rowid.insert(entity.label.clone(), id);
                self.embed_node(id, &format!("{}: {}", entity.label, entity.description)).await;
            }
        }

        for rel in &result.relations {
            let src_id = match label_to_rowid.get(&rel.source) {
                Some(id) => *id,
                None => continue,
            };
            let tgt_id = match label_to_rowid.get(&rel.target) {
                Some(id) => *id,
                None => continue,
            };

            let existing_edge: Option<(i64, Option<String>)> = self.db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, contexts FROM graph_edges WHERE source_id = ?1 AND target_id = ?2 AND label = ?3",
                )?;
                let mut rows = stmt.query(params![src_id, tgt_id, rel.label])?;
                if let Some(row) = rows.next()? {
                    Ok(Some((row.get(0)?, row.get(1)?)))
                } else {
                    Ok(None)
                }
            })?;

            if let Some((edge_id, ctx_json)) = existing_edge {
                let mut contexts: Vec<String> = ctx_json
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                for c in &rel.contexts {
                    if !contexts.contains(c) {
                        contexts.push(c.clone());
                    }
                }
                if contexts.len() > 20 {
                    contexts.truncate(20);
                }
                let ctx_str = serde_json::to_string(&contexts)?;

                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE graph_edges SET weight = weight + 1, contexts = ?1, updated_at = ?2, origin = '' WHERE id = ?3",
                        params![ctx_str, crate::db::utcnow(), edge_id],
                    )?;
                    Ok(())
                })?;
            } else {
                let contexts: Vec<String> = rel.contexts.iter().take(20).cloned().collect();
                let ctx_str = serde_json::to_string(&contexts)?;

                self.db.with_conn(|conn| {
                    conn.execute(
                        "INSERT INTO graph_edges (source_id, target_id, label, contexts, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![src_id, tgt_id, rel.label, ctx_str, now],
                    )?;
                    Ok(())
                })?;
                new_edges += 1;
            }
        }

        Ok((new_entities, updated_entities, new_edges))
    }

    async fn embed_node(&self, rowid: i64, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if let Ok(embs) = self.embedder.embed(&[text.to_string()]).await {
            if let Some(v) = embs.first() {
                let blob = serialize(v);
                let _ = self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE graph_nodes SET embedding = ?1 WHERE id = ?2",
                        params![blob, rowid],
                    )?;
                    Ok(())
                });
            }
        }
    }

    pub async fn search(&self, query: &str, limit: usize, expand_hops: bool) -> anyhow::Result<GraphSearchResult> {
        let words: Vec<String> = query
            .split_whitespace()
            .filter(|w| w.chars().count() >= 3)
            .map(|w| w.to_lowercase())
            .collect();

        let mut scored: HashMap<i64, f64> = HashMap::new();

        // 1. Полнотекстовый / лексический скоринг
        let all_nodes = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id, label, description FROM graph_nodes WHERE deleted_at IS NULL")?;
            let rows = stmt.query_map([], |row| {
                let id: i64 = row.get(0)?;
                let label: String = row.get(1)?;
                let desc: Option<String> = row.get(2)?;
                Ok((id, label, desc))
            })?;
            let mut list = Vec::new();
            for r in rows.flatten() {
                list.push(r);
            }
            Ok(list)
        })?;

        for (id, label, desc_opt) in all_nodes {
            let label_lower = label.to_lowercase();
            let desc_lower = desc_opt.unwrap_or_default().to_lowercase();
            let mut score = 0.0;
            for w in &words {
                if label_lower.contains(w) {
                    score += 10.0;
                } else if desc_lower.contains(w) {
                    score += 1.0;
                }
            }
            if score > 0.0 {
                scored.insert(id, score);
            }
        }

        // 2. Векторный скоринг
        if let Ok(q_embs) = self.embedder.embed(&[query.to_string()]).await {
            if let Some(q_vec) = q_embs.first() {
                let candidates = self.db.with_conn(|conn| {
                    let mut stmt = conn.prepare("SELECT id, embedding FROM graph_nodes WHERE embedding IS NOT NULL AND deleted_at IS NULL")?;
                    let rows = stmt.query_map([], |row| {
                        let id: i64 = row.get(0)?;
                        let blob: Vec<u8> = row.get(1)?;
                        Ok((id, blob))
                    })?;
                    let mut list = Vec::new();
                    for r in rows.flatten() {
                        list.push(r);
                    }
                    Ok(list)
                })?;

                let cand_refs: Vec<(i64, Option<&[u8]>)> = candidates.iter().map(|(id, b)| (*id, Some(b.as_slice()))).collect();
                for (nid, vscore) in top_k(q_vec, &cand_refs, limit, 0.0) {
                    let entry = scored.entry(nid).or_insert(0.0);
                    *entry += (vscore as f64) * 5.0;
                }
            }
        }

        let mut sorted_ids: Vec<i64> = scored.keys().copied().collect();
        sorted_ids.sort_by(|a, b| scored[b].partial_cmp(&scored[a]).unwrap_or(std::cmp::Ordering::Equal));
        if sorted_ids.len() > limit {
            sorted_ids.truncate(limit);
        }

        if sorted_ids.is_empty() {
            return Ok(GraphSearchResult::default());
        }

        // Извлекаем найденные узлы
        let mut nodes_map: HashMap<i64, NodeWithNeighbors> = HashMap::new();
        self.db.with_conn(|conn| {
            for &id in &sorted_ids {
                if let Ok(node) = conn.query_row(
                    "SELECT id, node_id, label, node_type, description, val FROM graph_nodes WHERE id = ?1",
                    params![id],
                    |row| {
                        Ok(NodeWithNeighbors {
                            id: row.get(0)?,
                            node_id: row.get(1)?,
                            label: row.get(2)?,
                            node_type: row.get(3)?,
                            description: row.get(4)?,
                            val: row.get(5)?,
                        })
                    },
                ) {
                    nodes_map.insert(id, node);
                }
            }
            Ok(())
        })?;

        // Извлекаем ребра
        let mut edges = Vec::new();
        let target_ids: Vec<i64> = nodes_map.keys().copied().collect();

        self.db.with_conn(|conn| {
            for &id in &target_ids {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT e.id, e.source_id, e.target_id, s.label, t.label, e.label, e.weight, e.contexts
                    FROM graph_edges e
                    JOIN graph_nodes s ON s.id = e.source_id
                    JOIN graph_nodes t ON t.id = e.target_id
                    WHERE (e.source_id = ?1 OR e.target_id = ?1) AND e.deleted_at IS NULL
                    "#,
                )?;
                let rows = stmt.query_map(params![id], |row| {
                    let ctx_str: Option<String> = row.get(7)?;
                    let contexts = ctx_str.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                    Ok(EdgeWithLabels {
                        id: row.get(0)?,
                        source_id: row.get(1)?,
                        target_id: row.get(2)?,
                        source_label: row.get(3)?,
                        target_label: row.get(4)?,
                        label: row.get(5)?,
                        weight: row.get(6)?,
                        contexts,
                    })
                })?;
                for r in rows.flatten() {
                    edges.push(r);
                }
            }
            Ok(())
        })?;

        // 1-hop расширение для добавления соседних узлов
        if expand_hops {
            for edge in &edges {
                for nid in [edge.source_id, edge.target_id] {
                    if !nodes_map.contains_key(&nid) {
                        if let Ok(neighbor) = self.db.with_conn(|conn| {
                            conn.query_row(
                                "SELECT id, node_id, label, node_type, description, val FROM graph_nodes WHERE id = ?1",
                                params![nid],
                                |row| {
                                    Ok(NodeWithNeighbors {
                                        id: row.get(0)?,
                                        node_id: row.get(1)?,
                                        label: row.get(2)?,
                                        node_type: row.get(3)?,
                                        description: row.get(4)?,
                                        val: row.get(5)?,
                                    })
                                },
                            )
                        }) {
                            nodes_map.insert(nid, neighbor);
                        }
                    }
                }
            }
        }

        Ok(GraphSearchResult {
            nodes: nodes_map.into_values().collect(),
            edges,
        })
    }

    pub async fn reason(&self, query: &str, llm: Arc<dyn LLMClient>) -> anyhow::Result<GraphReasonResult> {
        let found = self.search(query, 15, true).await?;
        if found.nodes.is_empty() {
            return Ok(GraphReasonResult {
                answer: "В графе нет данных по запросу.".to_string(),
                confidence: 0.0,
                reasoning_steps: Vec::new(),
                used_entities: Vec::new(),
                used_relations: Vec::new(),
                nodes_used: 0,
                edges_used: 0,
            });
        }

        let mut facts = vec!["Сущности:".to_string()];
        for n in &found.nodes {
            let desc = n.description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default();
            facts.push(format!("- {} ({}){}", n.label, n.node_type, desc));
        }

        facts.push("Отношения:".to_string());
        for e in found.edges.iter().take(40) {
            facts.push(format!("- {} --[{}]--> {}", e.source_label, e.label, e.target_label));
        }

        let facts_block = facts.join("\n");
        let prompt = format!("Вопрос: {query}\n\n{facts_block}");

        match llm.ask_json::<GraphReasonResult>(&prompt, Some(REASON_SYSTEM_PROMPT)).await {
            Ok(mut res) => {
                res.nodes_used = found.nodes.len();
                res.edges_used = found.edges.len();
                Ok(res)
            }
            Err(e) => {
                warn!("KAG reason JSON parse failed: {e}");
                Ok(GraphReasonResult {
                    answer: format!("[Error] LLM недоступен: {e}"),
                    confidence: 0.0,
                    reasoning_steps: Vec::new(),
                    used_entities: Vec::new(),
                    used_relations: Vec::new(),
                    nodes_used: found.nodes.len(),
                    edges_used: found.edges.len(),
                })
            }
        }
    }

    pub fn stats(&self) -> anyhow::Result<GraphStats> {
        self.db.with_conn(|conn| {
            let nodes: i64 = conn.query_row("SELECT count(*) FROM graph_nodes WHERE deleted_at IS NULL", [], |r| r.get(0))?;
            let edges: i64 = conn.query_row("SELECT count(*) FROM graph_edges WHERE deleted_at IS NULL", [], |r| r.get(0))?;
            let documents: i64 = conn.query_row("SELECT count(*) FROM documents", [], |r| r.get(0))?;
            let chunks: i64 = conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
            Ok(GraphStats {
                nodes,
                edges,
                documents,
                chunks,
            })
        })
    }
}
