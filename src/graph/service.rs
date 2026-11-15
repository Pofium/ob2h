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
use crate::vector::{serialize_q, top_k};

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

/// Ф29.2: узел в PPR-выдаче графа знаний.
#[derive(Debug, Clone, Serialize)]
pub struct PprNodeHit {
    pub node_id: i64,
    pub label: String,
    pub node_type: String,
    pub file_path: Option<String>,
    pub score: f64,
    pub val: i64,
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
                let subtokens = crate::db::schema::decompose_identifier(&entity.label);
                let id: i64 = self.db.with_conn(|conn| {
                    conn.execute(
                        "INSERT INTO graph_nodes (node_id, label, node_type, description, subtokens, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                        params![node_id, entity.label, entity.entity_type, entity.description, subtokens, now],
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
        if let Ok(embs) = self.embedder.embed_documents(&[text.to_string()]).await {
            if let Some(v) = embs.first() {
                let blob = serialize_q(v);
                let _ = self.db.with_conn(|conn| {
                    let means = crate::vector::bq::load_or_compute_means(conn, "graph_nodes", v.len());
                    let bq = crate::vector::bq::quantize_mean_centered(v, means.as_deref());
                    conn.execute(
                        "UPDATE graph_nodes SET embedding = ?1, bq_mask = ?2 WHERE id = ?3",
                        params![blob, bq, rowid],
                    )?;
                    if crate::vector::vec0::enabled() {
                        let _ = crate::vector::vec0::upsert_nodes_batch(
                            conn,
                            &[(rowid, v.as_slice())],
                            v.len(),
                            crate::vector::vec0::mode(),
                        );
                    }
                    Ok(())
                });
            }
        }
    }

    pub async fn search(&self, query: &str, limit: usize, expand_hops: bool) -> anyhow::Result<GraphSearchResult> {
        self.search_for_project(query, None, limit, expand_hops).await
    }

    /// Поиск по кодовому/знаниевому графу с опциональным партиционированием по `project_id` (Ф72b.3).
    pub async fn search_for_project(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: usize,
        expand_hops: bool,
    ) -> anyhow::Result<GraphSearchResult> {
        let decomposed_q = crate::db::schema::decompose_identifier(query);
        let mut seen_w: std::collections::HashSet<String> = std::collections::HashSet::new();
        let words: Vec<String> = decomposed_q
            .split_whitespace()
            .filter(|w| w.chars().count() >= 3)
            .map(|w| w.to_lowercase())
            .filter(|w| seen_w.insert(w.clone()))
            .collect();

        let mut scored: HashMap<i64, f64> = HashMap::new();

        // 1. Лексический скоринг. Ф62.1: кандидаты берём из FTS5 (BM25-совместимый
        //    поиск по подстроке через trigram-токенайзер), а не полным перебором
        //    `SELECT … FROM graph_nodes` + `LIKE`. Ранжирование (label/desc) прежнее.
        if !words.is_empty() {
            let match_expr = words
                .iter()
                .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" OR ");
            let fetch_all = |conn: &mut rusqlite::Connection| -> rusqlite::Result<Vec<(i64, String, Option<String>)>> {
                let mut stmt = conn.prepare(
                    "SELECT id, label, description FROM graph_nodes WHERE deleted_at IS NULL",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                Ok(rows.flatten().collect())
            };

            let candidates = match self.db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT n.id, n.label, n.description FROM graph_nodes n \
                     WHERE n.deleted_at IS NULL \
                       AND n.id IN (SELECT rowid FROM graph_nodes_fts WHERE graph_nodes_fts MATCH ?1)",
                )?;
                let rows = stmt.query_map(params![match_expr], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                Ok(rows.flatten().collect::<Vec<_>>())
            }) {
                Ok(list) => list,
                Err(e) => {
                    // FTS5 недоступен/индекс битый — прежний полный перебор (без регресса).
                    warn!("graph FTS5 недоступен, fallback на полный перебор: {e}");
                    self.db.with_conn(fetch_all)?
                }
            };

            for (id, label, desc_opt) in candidates {
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
        }

        // 2. Векторный скоринг
        if let Ok(q_vec) = self.embedder.embed_query(query).await {
            let q_vec = &q_vec;
                // Ф35.1: при OB2H_VEC0=1 (без фильтра project_id) — vec0;
                // иначе — партиционированный BQ two-stage по пространству AstNodes (Ф72b.3).
                let vec0_hits: Vec<(i64, f32)> = if project_id.is_none() && crate::vector::vec0::enabled() {
                    let os = crate::vector::vec0::oversample();
                    self.db
                        .with_conn(|conn| crate::vector::vec0::search(conn, q_vec, limit, os))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                let ranked: Vec<(i64, f32)> = if !vec0_hits.is_empty() {
                    vec0_hits
                } else {
                    self.db.with_conn(|conn| {
                        crate::vector::bq::search_namespace(
                            conn,
                            crate::vector::bq::VectorNamespace::AstNodes,
                            project_id,
                            q_vec,
                            limit,
                            0.0,
                        )
                    })?
                };

                for (nid, vscore) in ranked {
                    let entry = scored.entry(nid).or_insert(0.0);
                    *entry += (vscore as f64) * 5.0;
                }
            }

        let mut sorted_ids: Vec<i64> = scored.keys().copied().collect();
        // Ф54: тай-брейк по id — итерация HashMap случайна, без детерминизма
        // состав top-k (и сиды PPR) плавал между запусками при равных скорах
        sorted_ids.sort_by(|a, b| {
            scored[b]
                .partial_cmp(&scored[a])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
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
                    if let std::collections::hash_map::Entry::Vacant(e) = nodes_map.entry(nid) {
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
                            e.insert(neighbor);
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

    /// Ф29.2 (PLAN_v1.3): PPR-ранжирование графа знаний — сиды из существующего
    /// матчинга (FTS/лексика/вектор, без 1-hop), рёбра проекта с весами по типу
    /// (edge-type-aware, идеи HippoRAG 2 / GAAMA). Многошаговое: mass доходит
    /// до узлов, недостижимых за один хоп.
    /// Ф54: pk → node_id всех узлов проекта (для сопоставления с golden-набором).
    pub fn all_node_ids(
        &self,
        project_id: &str,
    ) -> anyhow::Result<Vec<(i64, String)>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, node_id FROM graph_nodes                  WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')",
            )?;
            let rows = stmt.query_map(params![project_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            Ok(rows.flatten().collect())
        })
    }

    pub async fn ppr_search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: usize,
        damping: f64,
        weights: &crate::graph::pagerank::PprWeights,
    ) -> anyhow::Result<Vec<PprNodeHit>> {
        const MAX_SUBGRAPH_NODES: usize = 8000;
        const MAX_HOPS: usize = 2;

        // Ф54 + Ф72b.3: окно сидов 50 с учётом проектного пространства имён.
        let found = self.search_for_project(query, project_id, 50, false).await?;
        if found.nodes.is_empty() {
            return Ok(Vec::new());
        }
        let seed_ids: Vec<i64> = found.nodes.iter().map(|n| n.id).collect();

        // Ф73.4: экстракция k-hop сабграфа (до 8000 узлов) от seeds для SLA < 20 мс
        let (nodes, raw_edges, dirty_files) = self.db.with_conn(|conn| {
            let mut visited: std::collections::HashSet<i64> = seed_ids.iter().copied().collect();
            let mut frontier: Vec<i64> = seed_ids.clone();
            let mut collected_edges: Vec<(i64, i64, String)> = Vec::new();

            for _hop in 0..MAX_HOPS {
                if frontier.is_empty() || visited.len() >= MAX_SUBGRAPH_NODES {
                    break;
                }
                let mut next_frontier: Vec<i64> = Vec::new();
                for chunk in frontier.chunks(400) {
                    let ph = vec!["?"; chunk.len()].join(",");
                    let sql = match project_id {
                        Some(_) => format!(
                            "SELECT source_id, target_id, label FROM graph_edges \
                             WHERE deleted_at IS NULL AND project_id = ?1 \
                               AND (source_id IN ({ph}) OR target_id IN ({ph}))"
                        ),
                        None => format!(
                            "SELECT source_id, target_id, label FROM graph_edges \
                             WHERE deleted_at IS NULL \
                               AND (source_id IN ({ph}) OR target_id IN ({ph}))"
                        ),
                    };
                    let mut stmt = conn.prepare(&sql)?;
                    let mut pvec: Vec<&dyn rusqlite::ToSql> = Vec::new();
                    if let Some(ref pid) = project_id {
                        pvec.push(pid);
                    }
                    for id in chunk {
                        pvec.push(id);
                    }
                    for id in chunk {
                        pvec.push(id);
                    }
                    let rows = stmt.query_map(pvec.as_slice(), |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })?;
                    for (s, t, lbl) in rows.flatten() {
                        collected_edges.push((s, t, lbl));
                        if visited.len() < MAX_SUBGRAPH_NODES && visited.insert(s) {
                            next_frontier.push(s);
                        }
                        if visited.len() < MAX_SUBGRAPH_NODES && visited.insert(t) {
                            next_frontier.push(t);
                        }
                    }
                }
                frontier = next_frontier;
            }

            let all_ids: Vec<i64> = visited.into_iter().collect();
            type SubNode = (i64, String, String, Option<String>, i64, i64, f64, String, String);
            let mut sub_nodes: Vec<SubNode> = Vec::with_capacity(all_ids.len());
            for chunk in all_ids.chunks(800) {
                let ph = vec!["?"; chunk.len()].join(",");
                let proj_filter = if project_id.is_some() {
                    " AND project_id = ?1"
                } else {
                    ""
                };
                let sql = format!(
                    "SELECT id, label, node_type, file_path, val, \
                     COALESCE(access_count, 0), COALESCE(trust_score, 1.0), \
                     COALESCE(status, 'ACTIVE'), created_at \
                     FROM graph_nodes \
                     WHERE deleted_at IS NULL{proj_filter} AND id IN ({ph})"
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut pvec: Vec<&dyn rusqlite::ToSql> = Vec::new();
                if let Some(ref pid) = project_id {
                    pvec.push(pid);
                }
                for id in chunk {
                    pvec.push(id);
                }
                let rows = stmt.query_map(pvec.as_slice(), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, f64>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                })?;
                for r in rows.flatten() {
                    sub_nodes.push(r);
                }
            }

            let dirty = if let Some(pid) = project_id {
                conn.query_row(
                    "SELECT root_path FROM projects WHERE id = ?1",
                    params![pid],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .map(|rp| crate::graph::pagerank::git_dirty_files(std::path::Path::new(&rp)))
                .unwrap_or_default()
            } else {
                std::collections::HashSet::new()
            };

            Ok((sub_nodes, collected_edges, dirty))
        })?;

        let index: std::collections::HashMap<i64, usize> =
            nodes.iter().enumerate().map(|(i, (id, ..))| (*id, i)).collect();

        // Считаем in_degree в сабграфе и возраст узлов
        let mut in_deg = vec![0usize; nodes.len()];
        for &(_, t, _) in &raw_edges {
            if let Some(&ti) = index.get(&t) {
                in_deg[ti] += 1;
            }
        }
        let now_utc = chrono::Utc::now();
        let age_days: Vec<f64> = nodes
            .iter()
            .map(|(.., created)| {
                chrono::DateTime::parse_from_rfc3339(created)
                    .map(|dt| (now_utc - dt.with_timezone(&chrono::Utc)).num_hours().max(0) as f64 / 24.0)
                    .unwrap_or(0.0)
            })
            .collect();

        // Ф73.5: исключаем SUPERSEDED узлы из целевого распределения PPR
        let superseded: Vec<bool> = nodes
            .iter()
            .map(|(_, _, _, _, _, _, _, status, _)| status.eq_ignore_ascii_case("SUPERSEDED"))
            .collect();

        let ppr_edges: Vec<crate::graph::pagerank::PprEdge> = raw_edges
            .iter()
            .filter_map(|(s, t, label)| {
                let &from = index.get(s)?;
                let &to = index.get(t)?;
                let target = &nodes[to];
                let w0 = crate::graph::pagerank::weight_for(weights, label);
                let is_code = target.3.is_some()
                    || matches!(target.2.as_str(), "Function" | "Struct" | "Class" | "Interface" | "Trait" | "Module" | "File");
                let w = crate::graph::pagerank::compute_usage_edge_weight(
                    crate::graph::pagerank::UsageEdgeParams {
                        w0,
                        access_count: target.5.max(0) as u64,
                        age_days: age_days[to],
                        is_code,
                        trust_score: target.6,
                        target_in_degree: in_deg[to],
                    },
                );
                Some(crate::graph::pagerank::PprEdge { from, to, weight: w })
            })
            .collect();

        let total_n = nodes.len().max(1);
        let mut seeds: Vec<(usize, f64)> = seed_ids
            .iter()
            .filter_map(|id| {
                let &idx = index.get(id)?;
                if superseded[idx] {
                    return None;
                }
                let trust = nodes[idx].6;
                let w = crate::graph::pagerank::compute_seed_idf_weight(total_n, in_deg[idx], trust);
                Some((idx, w))
            })
            .collect();
        if seeds.is_empty() {
            return Ok(Vec::new());
        }

        let node_paths: Vec<Option<&str>> = nodes.iter().map(|n| n.3.as_deref()).collect();
        crate::graph::pagerank::apply_working_set_boost(
            &mut seeds,
            &node_paths,
            &dirty_files,
            crate::graph::pagerank::WORKING_SET_BOOST,
        );

        let adj = crate::graph::pagerank::GraphAdjacency::from_edges(
            nodes.len(),
            &ppr_edges,
            Some(&superseded),
        );
        let ranked =
            crate::graph::pagerank::personalized_pagerank_csr(&adj, &seeds, damping, 20, 0.0);

        Ok(ranked
            .into_iter()
            .take(limit)
            .filter_map(|(idx, score)| {
                let (id, label, ntype, path, val, ..) = nodes.get(idx)?;
                Some(PprNodeHit {
                    node_id: *id,
                    label: label.clone(),
                    node_type: ntype.clone(),
                    file_path: path.clone(),
                    score,
                    val: *val,
                })
            })
            .collect())
    }

    /// Гибридный поиск узлов в графе проекта (FTS/LIKE + Vector Cosine через RRF k=60)
    pub async fn search_project_nodes_hybrid(
        &self,
        project_id: &str,
        query: &str,
        mode: &str,
        provenance: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ScoredProjectNode>> {
        let is_text_enabled = mode == "hybrid" || mode == "text";
        let is_vector_enabled = mode == "hybrid" || mode == "vector";

        // 1. Лексический поиск (по совпадению ключевых слов и подстрок)
        let mut text_ranks: HashMap<i64, usize> = HashMap::new();
        if is_text_enabled {
            let words: Vec<String> = query
                .split_whitespace()
                .filter(|w| w.chars().count() >= 2)
                .map(|w| w.to_lowercase())
                .collect();

            let candidates = self.db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT id, node_id, label, description, file_path
                    FROM graph_nodes
                    WHERE project_id = ?1
                      AND (?2 = 'all' OR provenance = ?2)
                      AND (deleted_at IS NULL OR deleted_at = '')
                    "#,
                )?;
                let rows = stmt.query_map(params![project_id, provenance], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })?;
                let mut list = Vec::new();
                for r in rows.flatten() {
                    list.push(r);
                }
                Ok(list)
            })?;

            let mut scored_text: Vec<(i64, f64)> = Vec::new();
            let query_lower = query.to_lowercase();
            for (id, node_id, label, desc_opt, fpath_opt) in candidates {
                let lbl = label.to_lowercase();
                // Ф48: node_id участвует в скоринге — узлы без «живого» label
                // (cssclass:/cssvar:/route:/pkg:/file:…) находятся по полному id
                let nid = node_id.to_lowercase();
                let desc = desc_opt.unwrap_or_default().to_lowercase();
                let fpath = fpath_opt.unwrap_or_default().to_lowercase();

                let mut score = 0.0;
                if lbl == query_lower || nid == query_lower {
                    score += 50.0;
                } else if lbl.contains(&query_lower) {
                    score += 25.0;
                } else if nid.contains(&query_lower) {
                    score += 20.0;
                } else if desc.contains(&query_lower) || fpath.contains(&query_lower) {
                    score += 15.0;
                }

                for w in &words {
                    if lbl.contains(w) {
                        score += 8.0;
                    }
                    if desc.contains(w) {
                        score += 3.0;
                    }
                    if fpath.contains(w) {
                        score += 2.0;
                    }
                    if nid.contains(w) {
                        score += 5.0;
                    }
                }

                if score > 0.0 {
                    scored_text.push((id, score));
                }
            }

            scored_text.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (rank, (id, _)) in scored_text.into_iter().enumerate() {
                text_ranks.insert(id, rank);
            }
        }

        // 2. Векторный семантический поиск
        let mut vector_ranks: HashMap<i64, usize> = HashMap::new();
        if is_vector_enabled {
            if let Ok(q_vec) = self.embedder.embed_query(query).await {
                let q_vec = &q_vec;
                    let candidates = self.db.with_conn(|conn| {
                        let mut stmt = conn.prepare(
                            r#"
                            SELECT id, embedding
                            FROM graph_nodes
                            WHERE project_id = ?1
                              AND embedding IS NOT NULL
                              AND (?2 = 'all' OR provenance = ?2)
                              AND (deleted_at IS NULL OR deleted_at = '')
                            "#,
                        )?;
                        let rows = stmt.query_map(params![project_id, provenance], |r| {
                            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                        })?;
                        let mut list = Vec::new();
                        for r in rows.flatten() {
                            list.push(r);
                        }
                        Ok(list)
                    })?;

                    let cand_refs: Vec<(i64, Option<&[u8]>)> = candidates
                        .iter()
                        .map(|(id, b)| (*id, Some(b.as_slice())))
                        .collect();

                    let scored_vec = top_k(q_vec, &cand_refs, limit * 3, 0.0);
                    for (rank, (id, _)) in scored_vec.into_iter().enumerate() {
                        vector_ranks.insert(id, rank);
                    }
                }
            }

        // 3. Слияние через RRF (k = 60)
        let k = 60.0;
        let mut all_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
        all_ids.extend(text_ranks.keys());
        all_ids.extend(vector_ranks.keys());

        let mut final_scores: Vec<(i64, f64)> = Vec::new();
        for &id in &all_ids {
            let score = match mode {
                "text" => {
                    let r = text_ranks.get(&id).copied().unwrap_or(9999);
                    1.0 / (k + r as f64)
                }
                "vector" => {
                    let r = vector_ranks.get(&id).copied().unwrap_or(9999);
                    1.0 / (k + r as f64)
                }
                _ => {
                    let text_score = text_ranks.get(&id).map(|&r| 1.0 / (k + r as f64)).unwrap_or(0.0);
                    let vec_score = vector_ranks.get(&id).map(|&r| 1.0 / (k + r as f64)).unwrap_or(0.0);
                    text_score + vec_score
                }
            };
            final_scores.push((id, score));
        }

        final_scores.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let ta = text_ranks.get(&a.0).copied().unwrap_or(usize::MAX);
                    let tb = text_ranks.get(&b.0).copied().unwrap_or(usize::MAX);
                    ta.cmp(&tb)
                })
                .then_with(|| a.0.cmp(&b.0))
        });
        let top_ids: Vec<(i64, f64)> = final_scores.into_iter().take(limit).collect();

        if top_ids.is_empty() {
            return Ok(Vec::new());
        }

        // 4. Подгрузка полной информации по узлам
        let mut results = Vec::new();

        self.db.with_conn(|conn| {
            for (id, score) in &top_ids {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT id, node_id, label, node_type, file_path, line_start, provenance, is_god_node, description
                    FROM graph_nodes
                    WHERE id = ?1
                    "#,
                )?;
                if let Ok(node) = stmt.query_row(params![id], |r| {
                    Ok(ScoredProjectNode {
                        id: r.get(0)?,
                        node_id: r.get(1)?,
                        label: r.get(2)?,
                        node_type: r.get(3)?,
                        file_path: r.get(4)?,
                        line_start: r.get(5)?,
                        provenance: r.get(6)?,
                        is_god_node: r.get::<_, i64>(7).unwrap_or(0) == 1,
                        description: r.get(8)?,
                        score: *score,
                    })
                }) {
                    results.push(node);
                }
            }
            Ok(())
        })?;

        Ok(results)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredProjectNode {
    pub id: i64,
    pub node_id: String,
    pub label: String,
    pub node_type: String,
    pub file_path: Option<String>,
    pub line_start: Option<i64>,
    pub provenance: Option<String>,
    pub is_god_node: bool,
    pub description: Option<String>,
    pub score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::FakeEmbedding;

    #[tokio::test]
    async fn test_subtoken_fts_finds_camel_and_snake_case_identifiers() {
        let db = Database::in_memory().unwrap();
        let embedder = Arc::new(FakeEmbedding::new(8));
        let svc = GraphService::new(db.clone(), embedder);

        db.with_conn(|conn| {
            let s1 = crate::db::schema::decompose_identifier("EmbeddingWorkerPool");
            let s2 = crate::db::schema::decompose_identifier("parse_source_file");
            conn.execute(
                "INSERT INTO graph_nodes (node_id, label, node_type, description, subtokens, project_id, val, created_at, updated_at)
                 VALUES ('n1', 'EmbeddingWorkerPool', 'Struct', 'struct EmbeddingWorkerPool', ?1, 'p1', 1.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                        ('n2', 'parse_source_file', 'Function', 'fn parse_source_file()', ?2, 'p1', 1.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                params![s1, s2],
            )?;
            Ok(())
        })
        .unwrap();

        let res1 = svc.search_for_project("worker pool", Some("p1"), 5, false).await.unwrap();
        assert!(
            res1.nodes.iter().any(|n| n.label == "EmbeddingWorkerPool"),
            "Ожидали найти EmbeddingWorkerPool по запросу 'worker pool', получили: {:?}",
            res1.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );

        let res2 = svc.search_for_project("parse source", Some("p1"), 5, false).await.unwrap();
        assert!(
            res2.nodes.iter().any(|n| n.label == "parse_source_file"),
            "Ожидали найти parse_source_file по запросу 'parse source', получили: {:?}",
            res2.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }
}
