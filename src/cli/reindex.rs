//! CLI-команда `ob2h reindex --embeddings` (Ф72.4).
//! Пакетная переиндексация векторов памяти и AST-графа при смене модели/бэкенда/чанкера
//! с обновлением `EmbeddingFingerprint` в `kv`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use rusqlite::params;

use crate::db::Database;
use crate::embedding::fingerprint::{save_fingerprint, EmbeddingFingerprint};
use crate::embedding::EmbeddingProvider;
use crate::project::ast_backend::is_generated_stub;
use crate::project::chunker::extract_node_span_chunk;
use crate::vector::similarity::serialize_q;

#[derive(Debug, Clone)]
pub struct ReindexReport {
    pub memories_reindexed: usize,
    pub graph_nodes_reindexed: usize,
    pub fingerprint: String,
}

fn render_progress(label: &str, done: usize, total: usize) {
    if total == 0 {
        return;
    }
    let pct = ((done * 100) / total).min(100);
    let width = 24usize;
    let filled = (done * width) / total;
    let bar: String = (0..width)
        .map(|i| if i < filled { '=' } else { ' ' })
        .collect();
    eprint!("\r{label}: [{bar}] {pct:>3}% ({done}/{total})");
    let _ = std::io::stderr().flush();
    if done >= total {
        eprintln!();
    }
}

/// Пересчитать все векторы `memories` и `graph_nodes` под текущий `EmbeddingProvider`
/// и зафиксировать новый `EmbeddingFingerprint`.
pub async fn reindex_embeddings(
    db: &Database,
    embedder: &Arc<dyn EmbeddingProvider>,
    batch_size: usize,
    show_progress: bool,
) -> anyhow::Result<ReindexReport> {
    let batch = batch_size.clamp(1, 128);

    // 1. Memories
    let mem_rows: Vec<(i64, String)> = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, content FROM memories WHERE deleted_at IS NULL ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;

    let total_mem = mem_rows.len();
    let mut mem_done = 0usize;
    for chunk in mem_rows.chunks(batch) {
        let texts: Vec<String> = chunk.iter().map(|(_, c)| c.clone()).collect();
        let vecs = embedder.embed_documents(&texts).await?;
        db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            for ((id, _), v) in chunk.iter().zip(vecs.iter()) {
                let blob = serialize_q(v);
                tx.execute(
                    "UPDATE memories SET embedding = ?1 WHERE id = ?2",
                    params![blob, id],
                )?;
            }
            tx.commit()?;
            Ok(())
        })?;
        mem_done += chunk.len();
        if show_progress {
            render_progress("memories", mem_done, total_mem);
        }
    }

    // 2. Graph nodes (с AST-aware чанкингом 72.2a и пропуском stub-файлов 70.15)
    type GraphReindexRow = (
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<String>,
    );
    let node_rows: Vec<GraphReindexRow> = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            r#"
            SELECT g.id, g.node_type, g.label, g.file_path, g.description,
                   g.line_start, g.line_end, p.root_path
            FROM graph_nodes g
            LEFT JOIN projects p ON p.id = g.project_id
            WHERE (g.deleted_at IS NULL OR g.deleted_at = '')
            ORDER BY g.id
            "#,
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let row: GraphReindexRow = r?;
            let is_stub = row
                .3
                .as_deref()
                .map(is_generated_stub)
                .unwrap_or(false);
            if !is_stub {
                out.push(row);
            }
        }
        Ok(out)
    })?;

    let total_nodes = node_rows.len();
    let mut nodes_done = 0usize;
    for chunk in node_rows.chunks(batch) {
        let texts: Vec<String> = chunk
            .iter()
            .map(|(_, ntype, label, fpath, desc, ls, le, root)| {
                let root_pb = root.as_deref().map(PathBuf::from);
                extract_node_span_chunk(
                    root_pb.as_deref(),
                    fpath.as_deref(),
                    ntype,
                    label,
                    desc.as_deref(),
                    *ls,
                    *le,
                )
            })
            .collect();
        let vecs = embedder.embed_documents(&texts).await?;
        db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            for ((id, ..), v) in chunk.iter().zip(vecs.iter()) {
                let blob = serialize_q(v);
                tx.execute(
                    "UPDATE graph_nodes SET embedding = ?1 WHERE id = ?2",
                    params![blob, id],
                )?;
            }
            tx.commit()?;
            if crate::vector::vec0::enabled() {
                if let Some(dim) = vecs.first().map(|v| v.len()).filter(|&d| d > 0) {
                    let items: Vec<(i64, &[f32])> = chunk
                        .iter()
                        .zip(vecs.iter())
                        .map(|((id, ..), v)| (*id, v.as_slice()))
                        .collect();
                    let _ = crate::vector::vec0::upsert_nodes_batch(
                        conn,
                        &items,
                        dim,
                        crate::vector::vec0::mode(),
                    );
                }
            }
            Ok(())
        })?;
        nodes_done += chunk.len();
        if show_progress {
            render_progress("graph_nodes", nodes_done, total_nodes);
        }
    }

    let fp = EmbeddingFingerprint::from_provider(embedder.as_ref());
    save_fingerprint(db, &fp)?;

    Ok(ReindexReport {
        memories_reindexed: mem_done,
        graph_nodes_reindexed: nodes_done,
        fingerprint: fp.as_key_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::fingerprint::{check_fingerprint, FingerprintStatus};
    use crate::embedding::FakeEmbedding;

    #[tokio::test]
    async fn test_reindex_embeddings_updates_vectors_and_fingerprint() {
        let db = Database::in_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memories (key, content, category, importance, created_at, updated_at) VALUES ('k1', 'тест памяти', 'fact', 0.8, '2026-11-15T00:00:00Z', '2026-11-15T00:00:00Z')",
                [],
            )?;
            conn.execute(
                "INSERT INTO graph_nodes (node_id, label, node_type, file_path, provenance, created_at, updated_at) VALUES ('fn:a:f', 'f', 'Function', 'src/a.rs', 'SYNTAX', '2026-11-15T00:00:00Z', '2026-11-15T00:00:00Z')",
                [],
            )?;
            // Генерированный stub должен пропускаться
            conn.execute(
                "INSERT INTO graph_nodes (node_id, label, node_type, file_path, provenance, created_at, updated_at) VALUES ('fn:b:g', 'g', 'Function', 'lib/foo.g.dart', 'SYNTAX', '2026-11-15T00:00:00Z', '2026-11-15T00:00:00Z')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        // Старый несовместимый отпечаток
        let old_fp = EmbeddingFingerprint::new("old-model", "candle", "fp32", 384, "prefix_v0", "ast_v0");
        save_fingerprint(&db, &old_fp).unwrap();

        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedding::new(64));
        let cur_fp = EmbeddingFingerprint::from_provider(embedder.as_ref());
        assert!(matches!(
            check_fingerprint(&db, &cur_fp).unwrap(),
            FingerprintStatus::Mismatch { .. }
        ));

        let report = reindex_embeddings(&db, &embedder, 16, false).await.unwrap();
        assert_eq!(report.memories_reindexed, 1);
        assert_eq!(report.graph_nodes_reindexed, 1);
        assert_eq!(
            check_fingerprint(&db, &cur_fp).unwrap(),
            FingerprintStatus::Match
        );
    }
}
