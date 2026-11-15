//! Content-addressed кэш парсинга и векторизации на базе `blake3` (Ф72b.5).
//! Таблица `file_blobs (blake3_hash PK, parser_version, embedding_fingerprint, ast_json, vector_ids)`:
//! при переключении git-веток (`main` ↔ `feat/x`) совпадение blake3-хеша
//! восстанавливает CST/AST и векторы за миллисекунды без ре-парсинга и ONNX.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::project::ast::{AstEdge, AstNode, AstScanResult, CallSite};

/// Вычисляет hex-представление blake3-хеша содержимого файла.
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Сериализованный снимок AST одного файла.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFileAst {
    pub nodes: Vec<AstNode>,
    pub edges: Vec<AstEdge>,
    pub call_sites: Vec<CallSite>,
    pub file_size: usize,
    pub lines_count: usize,
    pub backend: String,
}

/// Запись таблицы `file_blobs`.
#[derive(Debug, Clone)]
pub struct FileBlobRecord {
    pub blake3_hash: String,
    pub rel_path: String,
    pub project_id: Option<String>,
    pub parser_version: String,
    pub embedding_fingerprint: String,
    pub ast_json: String,
    pub vector_ids: Vec<i64>,
}

/// Ищет запись в `file_blobs` по blake3-хешу и версии парсера.
pub fn lookup_blob(
    conn: &Connection,
    blake3_hash: &str,
    parser_version: &str,
) -> Option<FileBlobRecord> {
    let row = conn
        .query_row(
            "SELECT blake3_hash, rel_path, project_id, parser_version, \
             embedding_fingerprint, ast_json, vector_ids \
             FROM file_blobs WHERE blake3_hash = ?1 AND parser_version = ?2",
            params![blake3_hash, parser_version],
            |r| {
                let vec_json: String = r.get(6)?;
                let vector_ids: Vec<i64> = serde_json::from_str(&vec_json).unwrap_or_default();
                Ok(FileBlobRecord {
                    blake3_hash: r.get(0)?,
                    rel_path: r.get(1)?,
                    project_id: r.get(2)?,
                    parser_version: r.get(3)?,
                    embedding_fingerprint: r.get(4)?,
                    ast_json: r.get(5)?,
                    vector_ids,
                })
            },
        )
        .optional()
        .ok()??;
    Some(row)
}

/// Сохраняет или обновляет запись `file_blobs`.
pub fn upsert_blob(conn: &Connection, rec: &FileBlobRecord) -> rusqlite::Result<()> {
    let vec_json = serde_json::to_string(&rec.vector_ids).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT INTO file_blobs \
         (blake3_hash, rel_path, project_id, parser_version, embedding_fingerprint, ast_json, vector_ids) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(blake3_hash) DO UPDATE SET \
         rel_path = excluded.rel_path, \
         project_id = COALESCE(excluded.project_id, file_blobs.project_id), \
         parser_version = excluded.parser_version, \
         embedding_fingerprint = CASE WHEN excluded.embedding_fingerprint != '' \
             THEN excluded.embedding_fingerprint ELSE file_blobs.embedding_fingerprint END, \
         ast_json = excluded.ast_json, \
         vector_ids = CASE WHEN excluded.vector_ids != '[]' \
             THEN excluded.vector_ids ELSE file_blobs.vector_ids END",
        params![
            rec.blake3_hash,
            rec.rel_path,
            rec.project_id,
            rec.parser_version,
            rec.embedding_fingerprint,
            rec.ast_json,
            vec_json,
        ],
    )?;
    Ok(())
}

/// Пытается восстановить AST файла из `file_blobs` по `blake3_hex(bytes)`.
/// Возвращает `true`, если кэш попал и узлы/рёбра/call_sites добавлены в `out`.
pub fn try_restore_ast(
    conn: &Connection,
    rel_path: &str,
    bytes: &[u8],
    parser_version: &str,
    out: &mut AstScanResult,
) -> bool {
    let hash = blake3_hex(bytes);
    let Some(rec) = lookup_blob(conn, &hash, parser_version) else {
        return false;
    };
    let Ok(cached) = serde_json::from_str::<CachedFileAst>(&rec.ast_json) else {
        return false;
    };

    out.files_scanned += 1;
    out.lines_total += cached.lines_count;
    out.file_meta
        .insert(rel_path.to_string(), (cached.file_size, cached.lines_count));

    for mut n in cached.nodes {
        n.file_path = rel_path.to_string();
        out.nodes.push(n);
    }
    out.edges.extend(cached.edges);
    for mut cs in cached.call_sites {
        cs.rel_path = rel_path.to_string();
        out.call_sites.push(cs);
    }
    true
}

/// Сохраняет `file_blobs` для всех файлов из `scan_res.file_meta` после успешного сканирования.
pub fn save_blobs_for_scan(
    conn: &Connection,
    root: &Path,
    project_id: &str,
    parser_version: &str,
    embedding_fingerprint: &str,
    scan_res: &AstScanResult,
) -> rusqlite::Result<usize> {
    if scan_res.file_meta.is_empty() {
        return Ok(0);
    }

    // Группируем nodes и call_sites по rel_path за O(N)
    let mut nodes_by_file: HashMap<&str, Vec<&AstNode>> = HashMap::new();
    let mut node_to_file: HashMap<&str, &str> = HashMap::new();
    for n in &scan_res.nodes {
        nodes_by_file.entry(n.file_path.as_str()).or_default().push(n);
        node_to_file.insert(n.node_id.as_str(), n.file_path.as_str());
    }
    let mut edges_by_file: HashMap<&str, Vec<&AstEdge>> = HashMap::new();
    for e in &scan_res.edges {
        if let Some(&fpath) = node_to_file.get(e.source_node_id.as_str()) {
            edges_by_file.entry(fpath).or_default().push(e);
        }
    }
    let mut calls_by_file: HashMap<&str, Vec<&CallSite>> = HashMap::new();
    for cs in &scan_res.call_sites {
        calls_by_file.entry(cs.rel_path.as_str()).or_default().push(cs);
    }

    let mut saved = 0usize;
    let mut vec_stmt = conn.prepare_cached(
        "SELECT id FROM graph_nodes WHERE project_id = ?1 AND file_path = ?2 AND embedding IS NOT NULL",
    )?;
    for (rel_path, &(file_size, lines_count)) in &scan_res.file_meta {
        let abs = root.join(rel_path);
        let Ok(bytes) = std::fs::read(&abs) else {
            continue;
        };
        let hash = blake3_hex(&bytes);
        let vector_ids: Vec<i64> = vec_stmt
            .query_map(params![project_id, rel_path], |r| r.get(0))?
            .flatten()
            .collect();

        let snapshot = CachedFileAst {
            nodes: nodes_by_file
                .get(rel_path.as_str())
                .map(|v| v.iter().map(|n| (*n).clone()).collect())
                .unwrap_or_default(),
            edges: edges_by_file
                .get(rel_path.as_str())
                .map(|v| v.iter().map(|e| (*e).clone()).collect())
                .unwrap_or_default(),
            call_sites: calls_by_file
                .get(rel_path.as_str())
                .map(|v| v.iter().map(|c| (*c).clone()).collect())
                .unwrap_or_default(),
            file_size,
            lines_count,
            backend: "ast".to_string(),
        };
        let ast_json = serde_json::to_string(&snapshot).unwrap_or_default();
        upsert_blob(
            conn,
            &FileBlobRecord {
                blake3_hash: hash,
                rel_path: rel_path.clone(),
                project_id: Some(project_id.to_string()),
                parser_version: parser_version.to_string(),
                embedding_fingerprint: embedding_fingerprint.to_string(),
                ast_json,
                vector_ids,
            },
        )?;
        saved += 1;
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn test_file_blobs_roundtrip_and_parser_version_invalidation() {
        let db = Database::in_memory().expect("db");
        let content = b"pub fn branch_demo() -> i32 { 42 }\n";
        let hash = blake3_hex(content);

        let ast = CachedFileAst {
            nodes: vec![AstNode {
                node_id: "fn:src/lib.rs:branch_demo".into(),
                label: "branch_demo".into(),
                node_type: "Function".into(),
                description: "pub fn branch_demo() -> i32".into(),
                file_path: "src/lib.rs".into(),
                line_start: 1,
                line_end: 1,
            }],
            edges: Vec::new(),
            call_sites: Vec::new(),
            file_size: content.len(),
            lines_count: 1,
            backend: "regex".into(),
        };
        let ast_json = serde_json::to_string(&ast).unwrap();

        db.with_conn(|conn| {
            upsert_blob(
                conn,
                &FileBlobRecord {
                    blake3_hash: hash.clone(),
                    rel_path: "src/lib.rs".into(),
                    project_id: Some("p1".into()),
                    parser_version: "v2".into(),
                    embedding_fingerprint: "candle:minilm:384:ast_v1".into(),
                    ast_json: ast_json.clone(),
                    vector_ids: vec![101, 102],
                },
            )?;

            // Совпадение blake3 + parser_version восстанавливает AST без парсинга
            let mut out = AstScanResult::default();
            assert!(try_restore_ast(conn, "src/lib.rs", content, "v2", &mut out));
            assert_eq!(out.nodes.len(), 1);
            assert_eq!(out.nodes[0].label, "branch_demo");

            // Смена parser_version инвалидирует кэш-хит
            let mut out_miss = AstScanResult::default();
            assert!(!try_restore_ast(
                conn,
                "src/lib.rs",
                content,
                "v3",
                &mut out_miss
            ));
            Ok(())
        })
        .expect("blob test");
    }
}
