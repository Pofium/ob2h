//! Ф37 (трек C, PLAN_v1.4): repo-map под token budget (Aider-паттерн).
//!
//! Компактная карта «файл → символы с сигнатурами» кодового проекта, уложенная в
//! бюджет токенов. Ранжирование файлов — PPR по file-dependency графу
//! (переиспользование `pagerank.rs` Ф29/Ф33):
//! - прямые рёбра file→file (label IMPORTS, когда оба конца — File);
//! - ко-импортная связность: два файла, импортирующие один и тот же внешний
//!   модуль, получают ребро весом 1/|файлы модуля| (редкий импорт — сильная связь,
//!   TF-IDF-подобный сигнал; компенсирует отсутствие CALLS-рёбер в AST-сканере).
//!
//! Сиды PPR: файлы, совпадающие с `query` (LIKE), либо все файлы (глобальная
//! центральность — «God Nodes» файлов наверх). Бюджет: жадное заполнение
//! префиксом отсортированных блоков (для независимых блоков эквивалентно
//! бинарному поиску максимума; `fit_prefix` реализован именно как бинпоиск).
//!
//! Детерминизм: сортировка по (ppr desc, file_path asc), форматирование ppr
//! с фиксированной точностью.

use std::collections::HashMap;

use rusqlite::{params, Connection};

use super::pagerank::{personalized_pagerank, PprEdge, PprGraph};

/// ~4 символа на токен для кода (эмпирика Aider; консервативно).
pub const TOKEN_CHARS: usize = 4;

/// Максимум символов на файл в карте (защита от монструозных описаний).
const MAX_FILE_BLOCK_CHARS: usize = 4000;

#[derive(Debug, Clone)]
struct FileEntry {
    id: i64,
    path: String,
}

#[derive(Debug, Clone)]
struct SymbolLine {
    text: String,
}

/// Вытянуть «сырую» сигнатуру из описания AST-сканера:
/// `Функция \`foo\` (pub fn foo(a: T) -> R {) в src/x.rs` → `pub fn foo(a: T) -> R {`.
fn signature_from_description(label: &str, node_type: &str, description: &str) -> String {
    let desc = description.trim();
    let extracted = if let Some(open) = desc.find(" (") {
        if let Some(close_rel) = desc[open + 2..].rfind(") в ") {
            let sig = desc[open + 2..open + 2 + close_rel].trim();
            if !sig.is_empty() {
                Some(sig.to_string())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    match extracted {
        Some(sig) => {
            let sig = sig.trim_end_matches('{').trim_end();
            let _ = label;
            sig.to_string()
        }
        None => format!("{node_type} {label}"),
    }
}

/// Файлы проекта (id + путь), детерминированный порядок.
fn load_files(conn: &Connection, project_id: &str) -> rusqlite::Result<Vec<FileEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(NULLIF(file_path, ''), label) AS path
         FROM graph_nodes
         WHERE project_id = ?1 AND node_type = 'File' AND deleted_at IS NULL
         ORDER BY path",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(FileEntry {
            id: r.get(0)?,
            path: r.get(1)?,
        })
    })?;
    let mut files: Vec<FileEntry> = rows.collect::<Result<_, _>>()?;
    // Ф53.1 (PLAN_v1.7): в repo-map — только файлы кода; манифесты/стили/
    // инфраструктура (не парсятся символами) в token-budget карту не идут.
    files.retain(|f| {
        crate::project::ast::LangSpec::for_path(&f.path)
            .map(|s| s.parser.is_code())
            .unwrap_or(true)
    });
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Символы файла (через DEFINES file→symbol), одна строка на символ.
fn load_symbols(
    conn: &Connection,
    project_id: &str,
    file_id: i64,
    limit: usize,
) -> rusqlite::Result<Vec<SymbolLine>> {
    let mut stmt = conn.prepare(
        "SELECT s.label, s.node_type, COALESCE(s.description, '')
         FROM graph_edges d
         JOIN graph_nodes s ON s.id = d.target_id
         WHERE d.label = 'DEFINES' AND d.source_id = ?2
           AND s.project_id = ?1 AND s.deleted_at IS NULL
         ORDER BY s.node_type, s.label, s.id",
    )?;
    let rows = stmt.query_map(params![project_id, file_id], |r| {
        let label: String = r.get(0)?;
        let node_type: String = r.get(1)?;
        let description: String = r.get(2)?;
        Ok(signature_from_description(&label, &node_type, &description))
    })?;
    let mut lines: Vec<SymbolLine> = rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|sig| SymbolLine { text: sig })
        .collect();
    lines.truncate(limit);
    Ok(lines)
}

/// Построить карту репозитория под бюджет токенов.
/// `with_memory` (Ф40.3) подмешивает высокодоверительную память проекта
/// (trust/importance ≥ 0.7) первым блоком — она приоритетнее файла карты.
#[allow(clippy::too_many_arguments)]
pub fn build_repo_map(
    conn: &Connection,
    project_id: &str,
    query: Option<&str>,
    max_tokens: usize,
    with_memory: bool,
) -> Result<String, rusqlite::Error> {
    let files = load_files(conn, project_id)?;
    if files.is_empty() {
        return Ok(format!(
            "<repo_map project=\"{project_id}\">\n(нет сканированных файлов — выполните project_scan)\n</repo_map>\n"
        ));
    }

    // --- File-dependency граф -------------------------------------------------
    // file → множество импортируемых модулей (label ExternalModule) + прямые
    // file→file рёбра.
    let idx_of: HashMap<i64, usize> = files.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    let mut direct_edges: Vec<(usize, usize)> = Vec::new();
    let mut file_imports: HashMap<usize, Vec<String>> = HashMap::new();

    let mut all = conn.prepare(
        "SELECT e.source_id, e.target_id, COALESCE(t.node_type, ''), COALESCE(t.label, '')
         FROM graph_edges e
         LEFT JOIN graph_nodes t ON t.id = e.target_id
         WHERE e.label = 'IMPORTS' AND e.deleted_at IS NULL AND e.project_id = ?1",
    )?;
    let rows = all.query_map(params![project_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (src, tgt, ttype, tlabel) = row?;
        let Some(&si) = idx_of.get(&src) else {
            continue;
        };
        if ttype == "File" {
            if let Some(&ti) = idx_of.get(&tgt) {
                if si != ti {
                    direct_edges.push((si, ti));
                }
            }
        } else if ttype == "ExternalModule" {
            file_imports.entry(si).or_default().push(tlabel);
        }
    }

    // Ко-импортная связность: пары файлов одного модуля, вес 1/|файлы модуля|.
    let mut module_files: HashMap<String, Vec<usize>> = HashMap::new();
    let mut imports: Vec<(usize, String)> = file_imports
        .iter()
        .flat_map(|(&f, ms)| ms.iter().map(move |m| (f, m.clone())))
        .collect();
    imports.sort();
    imports.dedup();
    for (f, m) in imports {
        module_files.entry(m).or_default().push(f);
    }
    let mut pair_weight: HashMap<(usize, usize), f64> = HashMap::new();
    for (_, mut fs) in module_files {
        fs.sort();
        fs.dedup();
        let w = 1.0 / fs.len() as f64;
        for i in 0..fs.len() {
            for j in (i + 1)..fs.len() {
                let key = (fs[i].min(fs[j]), fs[i].max(fs[j]));
                *pair_weight.entry(key).or_default() += w;
            }
        }
    }

    // --- PPR ------------------------------------------------------------------
    let n = files.len();
    let mut edges: Vec<PprEdge> = Vec::new();
    for (si, ti) in &direct_edges {
        edges.push(PprEdge {
            from: *si,
            to: *ti,
            weight: 1.0,
        });
    }
    for ((a, b), w) in &pair_weight {
        // недиапазонные веса нормирует PprGraph; кладём оба направления
        edges.push(PprEdge {
            from: *a,
            to: *b,
            weight: *w,
        });
        edges.push(PprEdge {
            from: *b,
            to: *a,
            weight: *w,
        });
    }
    let graph = PprGraph::new(n, &edges);

    // Сиды: query → совпавшие файлы; иначе все файлы (глобальная центральность).
    let seeds: Vec<(usize, f64)> = match query {
        Some(q) if !q.trim().is_empty() => {
            let like = format!("%{}%", q.trim());
            let mut stmt = conn.prepare(
                "SELECT id FROM graph_nodes
                 WHERE project_id = ?1 AND node_type = 'File' AND deleted_at IS NULL
                   AND (label LIKE ?2 OR file_path LIKE ?2)",
            )?;
            let rows = stmt.query_map(params![project_id, like], |r| r.get::<_, i64>(0))?;
            let mut seeds: Vec<(usize, f64)> = rows
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter_map(|id| idx_of.get(&id).map(|&i| (i, 1.0)))
                .collect();
            seeds.sort_by_key(|(i, _)| *i);
            seeds.dedup_by_key(|(i, _)| *i);
            if seeds.is_empty() {
                (0..n).map(|i| (i, 1.0)).collect()
            } else {
                seeds
            }
        }
        _ => (0..n).map(|i| (i, 1.0)).collect(),
    };

    let ranked = personalized_pagerank(&graph, &seeds, 0.85, 100, 1e-6);

    // --- Блоки файлов -----------------------------------------------------------
    let mut blocks: Vec<String> = Vec::with_capacity(n);

    // Ф40.3: высокодоверительная память проекта — первым блоком
    if with_memory {
        let mut mstmt = conn.prepare(
            "SELECT key, content FROM memories
             WHERE project_id = ?1 AND deleted_at IS NULL
               AND (trust >= 0.7 OR importance >= 0.7)
             ORDER BY updated_at DESC LIMIT 5",
        )?;
        let rows = mstmt
            .query_map(params![project_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if !rows.is_empty() {
            let mut block = String::from("## Память проекта (high-trust)\n");
            for (key, content) in rows {
                let short: String = content.chars().take(120).collect();
                block.push_str(&format!("- **{}**: {}\n", key, short));
            }
            block.truncate(MAX_FILE_BLOCK_CHARS);
            blocks.push(block);
        }
    }

    for &(node, score) in &ranked {
        let file = &files[node];
        let syms = load_symbols(conn, project_id, file.id, 12)?;
        if syms.is_empty() {
            continue; // файл без символов не даёт карте ценности
        }
        let mut block = format!("## {} (ppr {:.4})\n", file.path, score);
        for s in &syms {
            block.push_str(&format!("- {}\n", s.text));
        }
        block.truncate(MAX_FILE_BLOCK_CHARS);
        blocks.push(block);
    }

    // --- Бюджет: бинарный поиск максимального префикса --------------------------
    let budget_chars = max_tokens.saturating_mul(TOKEN_CHARS);
    let header = format!(
        "<repo_map project=\"{project_id}\" files_ranked={} budget={max_tokens}tok>\n",
        blocks.len()
    );
    let footer = "</repo_map>\n";
    // Резерв под служебный хвост «(... ещё N ...)» + «(used=...)» — они добавляются
    // после подбора префикса и не должны выталкивать карту за бюджет.
    const TAIL_RESERVE: usize = 128;
    let fixed = header.len() + footer.len() + TAIL_RESERVE;
    let k = fit_prefix(&blocks, budget_chars.saturating_sub(fixed));
    let mut out = header;
    for b in &blocks[..k] {
        out.push_str(b);
    }
    let used_tokens = out.len().div_ceil(TOKEN_CHARS);
    if k < blocks.len() {
        out.push_str(&format!(
            "(... ещё {} файлов не влезли в бюджет; поднимите max_tokens)\n",
            blocks.len() - k
        ));
    }
    out.push_str(&format!("(used={used_tokens}tok)\n"));
    out.push_str(footer);
    Ok(out)
}

/// Максимальный префикс блоков, суммарная длина которого ≤ budget.
/// Бинарный поиск по длине префикса (блоки независимы → префикс оптимален).
fn fit_prefix(blocks: &[String], budget: usize) -> usize {
    if blocks.is_empty() || budget == 0 {
        return 0;
    }
    let fits = |k: usize| -> bool { blocks[..k].iter().map(|b| b.len()).sum::<usize>() <= budget };
    let (mut lo, mut hi) = (0usize, blocks.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Узел, ранжированный U-PPR, для рендера токен-бюджетного AST-скелета (73.8).
#[derive(Debug, Clone)]
pub struct SkeletonNode {
    pub node_id: i64,
    pub label: String,
    pub node_type: String,
    pub score: f64,
}

/// Порог отсечения мусорного хвоста (Elbow / Drop-off Cutoff, 73.8):
/// остановка, если `score < 0.15 * s_max` или соседний скачок вниз > 50%.
pub fn apply_elbow_cutoff(hits: &[SkeletonNode]) -> Vec<SkeletonNode> {
    if hits.is_empty() {
        return Vec::new();
    }
    let s_max = hits[0].score;
    if s_max <= 0.0 {
        return Vec::new();
    }
    let mut kept = Vec::with_capacity(hits.len());
    kept.push(hits[0].clone());
    for i in 1..hits.len() {
        let cur = hits[i].score;
        let prev = hits[i - 1].score;
        if cur < 0.15 * s_max {
            break;
        }
        if prev > 0.0 && (prev - cur) / prev > 0.50 {
            break;
        }
        kept.push(hits[i].clone());
    }
    kept
}

/// Рендер компактного AST-скелета «файл → сигнатура → вызываемые зависимости (weight)»,
/// упакованного строго под `max_tokens` с учётом Elbow/Drop-off Cutoff (73.8).
pub fn render_ast_skeleton(
    conn: &Connection,
    project_id: Option<&str>,
    hits: &[SkeletonNode],
    max_tokens: usize,
) -> anyhow::Result<String> {
    if max_tokens == 0 || hits.is_empty() {
        return Ok(String::new());
    }
    let filtered = apply_elbow_cutoff(hits);
    if filtered.is_empty() {
        return Ok(String::new());
    }
    let budget_chars = max_tokens.saturating_mul(TOKEN_CHARS);
    if budget_chars == 0 {
        return Ok(String::new());
    }

    let mut node_stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(file_path, ''), ''), COALESCE(description, '')
         FROM graph_nodes
         WHERE id = ?1 AND deleted_at IS NULL
           AND (?2 IS NULL OR project_id = ?2)",
    )?;
    let mut edge_stmt = conn.prepare(
        "SELECT e.label, t.label, e.weight
         FROM graph_edges e
         JOIN graph_nodes t ON t.id = e.target_id
         WHERE e.source_id = ?1 AND t.deleted_at IS NULL
         ORDER BY e.weight DESC, t.label ASC
         LIMIT 5",
    )?;

    let mut blocks: Vec<String> = Vec::with_capacity(filtered.len());
    for hit in &filtered {
        let info: Option<(String, String)> = node_stmt
            .query_row(params![hit.node_id, project_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .ok();
        let (file_path, desc) = info.unwrap_or_default();
        let sig = signature_from_description(&hit.label, &hit.node_type, &desc);
        let loc = if file_path.is_empty() {
            "<graph>".to_string()
        } else {
            file_path
        };

        let mut block = format!("{loc} -> {sig} [ppr={:.4}]\n", hit.score);
        let edges = edge_stmt
            .query_map(params![hit.node_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (elabel, tlabel, weight) in edges {
            block.push_str(&format!("  -> {elabel} {tlabel} (conf={weight:.2})\n"));
        }
        if block.len() > MAX_FILE_BLOCK_CHARS {
            let mut end = MAX_FILE_BLOCK_CHARS;
            while end > 0 && !block.is_char_boundary(end) {
                end -= 1;
            }
            block.truncate(end);
            block.push('\n');
        }
        blocks.push(block);
    }

    let k = fit_prefix(&blocks, budget_chars);
    if k == 0 {
        // Даже первый блок шире бюджета: отдаём аккуратно обрезанный префикс первого блока.
        let mut s = blocks[0].clone();
        if s.len() > budget_chars {
            let mut end = budget_chars;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
        }
        return Ok(s.trim_end().to_string());
    }
    Ok(blocks[..k].concat().trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn mk_hit(id: i64, label: &str, score: f64) -> SkeletonNode {
        SkeletonNode {
            node_id: id,
            label: label.to_string(),
            node_type: "Function".to_string(),
            score,
        }
    }

    #[test]
    fn test_elbow_cutoff_stops_on_tail_and_sharp_drop() {
        // Резкий скачок вниз > 50% (0.80 -> 0.35 = 56.25%) обрезает хвост.
        let hits = vec![
            mk_hit(1, "a", 1.0),
            mk_hit(2, "b", 0.80),
            mk_hit(3, "c", 0.35),
            mk_hit(4, "d", 0.30),
        ];
        let kept = apply_elbow_cutoff(&hits);
        assert_eq!(kept.len(), 2);

        // Порог < 0.15 * s_max отсекает мусорный хвост даже при плавном снижении.
        let hits2 = vec![
            mk_hit(1, "a", 1.0),
            mk_hit(2, "b", 0.60),
            mk_hit(3, "c", 0.35),
            mk_hit(4, "d", 0.20),
            mk_hit(5, "e", 0.14),
        ];
        let kept2 = apply_elbow_cutoff(&hits2);
        assert_eq!(kept2.len(), 4);
    }

    #[test]
    fn test_render_ast_skeleton_respects_max_tokens_and_renders_calls() {
        let db = Database::in_memory().unwrap();
        let conn = db.conn_arc();
        let conn = conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, node_id, label, node_type, description, file_path, project_id, val, created_at, updated_at)
             VALUES (1, 'n1', 'handle_auth', 'Function', 'Функция `handle_auth` (pub fn handle_auth(tok: &str) -> bool {) в src/auth.rs', 'src/auth.rs', 'p1', 1.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                    (2, 'n2', 'verify_jwt', 'Function', 'Функция `verify_jwt` (fn verify_jwt(s: &str) -> bool {) в src/jwt.rs', 'src/jwt.rs', 'p1', 1.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                    (3, 'n3', 'noise_fn', 'Function', 'Функция `noise_fn` (fn noise_fn() {) в src/noise.rs', 'src/noise.rs', 'p1', 1.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, label, weight, created_at)
             VALUES (1, 2, 'CALLS', 0.92, '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let hits = vec![
            mk_hit(1, "handle_auth", 0.90),
            mk_hit(2, "verify_jwt", 0.70),
            mk_hit(3, "noise_fn", 0.05), // < 0.15 * 0.90 -> отсечён Elbow Cutoff
        ];
        let out = render_ast_skeleton(&conn, Some("p1"), &hits, 1024).unwrap();
        assert!(out.contains("src/auth.rs -> pub fn handle_auth(tok: &str) -> bool"));
        assert!(out.contains("-> CALLS verify_jwt (conf=0.92)"));
        assert!(!out.contains("noise_fn"));

        // Жёсткий бюджет токенов (например, 20 токенов = 80 байт) не превышается.
        let tight = render_ast_skeleton(&conn, Some("p1"), &hits, 20).unwrap();
        assert!(tight.len() <= 20 * TOKEN_CHARS);
    }
}
