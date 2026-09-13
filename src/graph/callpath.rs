//! Ф36 (трек C, PLAN_v1.4): structural queries по кодовому графу —
//! call-path (цепочки вызовов/зависимостей), callers/callees и dead-code.
//!
//! Всё поверх существующих `graph_nodes`/`graph_edges` (без новых СУБД и без
//! tree-sitter, ADR-1/§7). «Использованием» символа считаются рёбра
//! `CALLS`/`IMPORTS`/`IMPLEMENTS`/`DEPENDS_ON` — служебные рёбра `DEFINES`
//! (файл → символ) в in-degree не входят, иначе «мёртвыми» не были бы никто.

use rusqlite::{params, Connection};
use serde::Serialize;

/// Метки рёбер, означающие реальное использование символа.
pub const USAGE_EDGE_LABELS: &[&str] = &["CALLS", "IMPORTS", "IMPLEMENTS", "DEPENDS_ON"];

/// Типы узлов, которые вообще могут быть «символами» для dead-code.
const SYMBOL_TYPES: &[&str] = &[
    "Function",
    "Struct",
    "Class",
    "Interface",
    "Trait",
    "Method",
];

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRef {
    pub id: i64,
    pub node_id: String,
    pub label: String,
    pub node_type: String,
    pub file_path: Option<String>,
    pub line_start: Option<i64>,
}

impl SymbolRef {
    pub fn location(&self) -> String {
        match (&self.file_path, self.line_start) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            (Some(f), None) => f.clone(),
            _ => "-".to_string(),
        }
    }
}

/// Один шаг обхода: символ + ребро, по которому он достигнут + глубина.
#[derive(Debug, Clone, Serialize)]
pub struct CallLink {
    pub symbol: SymbolRef,
    pub edge: String,
    /// Ф39.3: происхождение ребра (M8) — агент калибрует доверие к шуму
    /// (RESOLVED — type-pass, EXTRACTED/'ast' — обычный скан, INFERRED — эвристики).
    pub provenance: String,
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallPathResult {
    pub from: SymbolRef,
    pub to: SymbolRef,
    /// Цепочка «A —CALLS→ B —CALLS→ C»
    pub hops: usize,
    pub chain: Vec<CallLink>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeadSymbol {
    pub symbol: SymbolRef,
    /// Почему символ попал в отчёт (диагностическая строка)
    pub incoming_usage_edges: i64,
}

/// Направление обхода рёбер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Кто вызывает/импортирует этот символ (входящие рёбра)
    Callers,
    /// Кого вызывает/импортирует этот символ (исходящие рёбра)
    Callees,
}

fn row_to_symbol(r: &rusqlite::Row) -> rusqlite::Result<SymbolRef> {
    Ok(SymbolRef {
        id: r.get(0)?,
        node_id: r.get(1)?,
        label: r.get(2)?,
        node_type: r.get(3)?,
        file_path: r.get(4)?,
        line_start: r.get(5)?,
    })
}

/// Найти узел-символ по имени (точное совпадение, затем LIKE, затем путь).
pub fn resolve_symbol(
    conn: &Connection,
    project_id: &str,
    symbol: &str,
) -> rusqlite::Result<Option<SymbolRef>> {
    let pattern = format!("%{symbol}%");
    let mut stmt = conn.prepare(
        "SELECT id, node_id, label, node_type, file_path, line_start
         FROM graph_nodes
         WHERE project_id = ?1
           AND (deleted_at IS NULL OR deleted_at = '')
           AND (label = ?2 OR label LIKE ?3 OR file_path = ?2 OR file_path LIKE ?3)
         ORDER BY CASE WHEN label = ?2 THEN 0 WHEN file_path = ?2 THEN 1 ELSE 2 END,
                  LENGTH(label)
         LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![project_id, symbol, pattern], row_to_symbol)?;
    match rows.next() {
        Some(Ok(s)) => Ok(Some(s)),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

fn symbol_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<SymbolRef>> {
    let mut stmt = conn.prepare(
        "SELECT id, node_id, label, node_type, file_path, line_start
         FROM graph_nodes WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], row_to_symbol)?;
    match rows.next() {
        Some(Ok(s)) => Ok(Some(s)),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

fn labels_clause() -> String {
    USAGE_EDGE_LABELS
        .iter()
        .map(|l| format!("'{l}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Обход в ширину по рёбрам использования: callers (входящие) или callees
/// (исходящие) до `depth` шагов. Найденное дедуплицируется по узлу (первое
/// вхождение = кратчайший путь).
pub fn neighbors(
    conn: &Connection,
    project_id: &str,
    start_id: i64,
    dir: Dir,
    depth: usize,
    limit: usize,
) -> rusqlite::Result<Vec<CallLink>> {
    let depth = depth.clamp(1, 10);
    let labels = labels_clause();
    let sql = match dir {
        Dir::Callers => format!(
            "SELECT e.source_id, e.label, COALESCE(e.provenance, '') FROM graph_edges e
             JOIN graph_nodes n ON n.id = e.source_id
             WHERE e.target_id = ?1 AND e.label IN ({labels})
               AND n.project_id = ?2 AND (n.deleted_at IS NULL OR n.deleted_at = '')"
        ),
        Dir::Callees => format!(
            "SELECT e.target_id, e.label, COALESCE(e.provenance, '') FROM graph_edges e
             JOIN graph_nodes n ON n.id = e.target_id
             WHERE e.source_id = ?1 AND e.label IN ({labels})
               AND n.project_id = ?2 AND (n.deleted_at IS NULL OR n.deleted_at = '')"
        ),
    };

    let mut out: Vec<CallLink> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = Default::default();
    seen.insert(start_id);
    let mut frontier = vec![start_id];
    for level in 1..=depth {
        let mut next = Vec::new();
        for node in frontier {
            let mut stmt = conn.prepare(&sql)?;
            let found = stmt.query_map(params![node, project_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for f in found {
                let (id, edge, provenance) = f?;
                if !seen.insert(id) {
                    continue;
                }
                if let Some(sym) = symbol_by_id(conn, id)? {
                    out.push(CallLink {
                        symbol: sym,
                        edge,
                        provenance,
                        depth: level,
                    });
                    if out.len() >= limit {
                        return Ok(out);
                    }
                    next.push(id);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(out)
}

/// Явная цепочка от `from` до `to` (BFS с реконструкцией пути по parents).
pub fn call_path(
    conn: &Connection,
    project_id: &str,
    from: &SymbolRef,
    to: &SymbolRef,
    max_depth: usize,
) -> rusqlite::Result<Option<CallPathResult>> {
    let max_depth = max_depth.clamp(1, 12);
    let labels = labels_clause();
    let sql = format!(
        "SELECT e.target_id, e.label, COALESCE(e.provenance, '') FROM graph_edges e
         JOIN graph_nodes n ON n.id = e.target_id
         WHERE e.source_id = ?1 AND e.label IN ({labels})
           AND n.project_id = ?2 AND (n.deleted_at IS NULL OR n.deleted_at = '')"
    );

    let mut parent: std::collections::HashMap<i64, (i64, String, String)> = Default::default();
    let mut seen: std::collections::HashSet<i64> = Default::default();
    seen.insert(from.id);
    let mut frontier = vec![from.id];
    let mut found = false;

    for _ in 0..max_depth {
        let mut next = Vec::new();
        for node in frontier {
            let mut stmt = conn.prepare(&sql)?;
            let out = stmt.query_map(params![node, project_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in out {
                let (target, edge, provenance) = row?;
                if !seen.insert(target) {
                    continue;
                }
                parent.insert(target, (node, edge, provenance));
                if target == to.id {
                    found = true;
                    break;
                }
                next.push(target);
            }
            if found {
                break;
            }
        }
        if found || next.is_empty() {
            break;
        }
        frontier = next;
    }

    if !found {
        return Ok(None);
    }

    // реконструкция: от to к from
    let mut chain_ids: Vec<(i64, String, String)> = Vec::new();
    let mut cursor = to.id;
    while cursor != from.id {
        let (prev, edge, prov) = match parent.get(&cursor) {
            Some(v) => v.clone(),
            None => break,
        };
        chain_ids.push((cursor, edge, prov));
        cursor = prev;
    }
    chain_ids.reverse();

    let mut chain = Vec::new();
    for (idx, (id, edge, prov)) in chain_ids.iter().enumerate() {
        if let Some(sym) = symbol_by_id(conn, *id)? {
            chain.push(CallLink {
                symbol: sym,
                edge: edge.clone(),
                provenance: prov.clone(),
                depth: idx + 1,
            });
        }
    }
    Ok(Some(CallPathResult {
        from: from.clone(),
        to: to.clone(),
        hops: chain.len(),
        chain,
    }))
}

/// Правило entrypoint'а: почему символ НЕ считается мёртвым (None = обычный символ).
/// Список расширяется в Ф40 (kind=ROUTE/HANDLES).
pub fn entrypoint_reason(sym: &SymbolRef, description: Option<&str>) -> Option<String> {
    let file = sym.file_path.clone().unwrap_or_default();
    let file_l = file.to_lowercase();
    let label = sym.label.as_str();
    let desc = description.unwrap_or("").to_lowercase();

    if label == "main" || label == "Main" || label.ends_with("::main") {
        return Some("entrypoint: main".to_string());
    }
    // тестовый код: каталог/файл по маскам или имя функции по префиксу
    if file_l.contains("tests/") || file_l.contains("/test") || file_l.contains("_test.") {
        return Some("test code (путь)".to_string());
    }
    if file_l.starts_with("test_") || file_l.contains("/test_") {
        return Some("test code (файл)".to_string());
    }
    if label.starts_with("test_") || label.starts_with("should_") || label.starts_with("Test") {
        return Some("test code (имя)".to_string());
    }
    // публичный API: сигнатура в описании узла (`pub fn` / `export`)
    if desc.contains("pub fn")
        || desc.contains("pub async fn")
        || desc.contains("pub struct")
        || desc.contains("pub enum")
        || desc.contains("pub trait")
        || desc.contains("export ")
    {
        return Some("public API".to_string());
    }
    None
}

/// Dead code: символы без входящих рёбер использования, кроме entrypoints.
/// Возвращает (символ, число входящих usage-рёбер).
pub fn dead_code(conn: &Connection, project_id: &str) -> rusqlite::Result<Vec<DeadSymbol>> {
    let labels = labels_clause();
    let types = SYMBOL_TYPES
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT n.id, n.node_id, n.label, n.node_type, n.file_path, n.line_start, n.description,
                COALESCE((SELECT COUNT(*) FROM graph_edges e
                          WHERE e.target_id = n.id AND e.label IN ({labels})), 0) AS indeg
         FROM graph_nodes n
         WHERE n.project_id = ?1
           AND n.node_type IN ({types})
           AND (n.deleted_at IS NULL OR n.deleted_at = '')
           AND NOT EXISTS (
               SELECT 1 FROM graph_edges fe
               WHERE fe.target_id = n.id
                 AND fe.label IN ('ROUTE', 'HANDLES')
                 AND fe.deleted_at IS NULL
           )
         ORDER BY indeg, n.file_path, n.line_start"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok((
            row_to_symbol(r)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (sym, desc, indeg) = row?;
        if indeg > 0 {
            continue;
        }
        if entrypoint_reason(&sym, desc.as_deref()).is_some() {
            continue;
        }
        out.push(DeadSymbol {
            symbol: sym,
            incoming_usage_edges: indeg,
        });
    }
    Ok(out)
}

/// Markdown-представление callers/callees.
pub fn format_links(header: &str, links: &[CallLink]) -> String {
    if links.is_empty() {
        return format!("{header}: ничего не найдено");
    }
    let mut s = format!("{header} ({}):\n", links.len());
    for l in links {
        s.push_str(&format!(
            "- {} `{}` [{}] —{}({})→ глубина {}\n",
            l.symbol.location(),
            l.symbol.label,
            l.symbol.node_type,
            l.edge,
            l.provenance,
            l.depth
        ));
    }
    s
}

/// Markdown-представление цепочки вызовов.
pub fn format_path(res: &CallPathResult) -> String {
    let mut chain = format!("`{}`", res.from.label);
    for l in &res.chain {
        chain.push_str(&format!(" —{}→ `{}`", l.edge, l.symbol.label));
    }
    format!(
        "Путь ({} шагов) от {} к {}:\n{chain}",
        res.hops,
        res.from.location(),
        res.to.location()
    )
}

/// Markdown-представление dead-code отчёта.
pub fn format_dead_code(symbols: &[DeadSymbol], limit: usize) -> String {
    if symbols.is_empty() {
        return "Мёртвых символов не найдено (in-degree 0, кроме entrypoints)".to_string();
    }
    let mut s = format!(
        "Кандидаты в мёртвый код (in-degree 0 по CALLS/IMPORTS/IMPLEMENTS/DEPENDS_ON): {}\n",
        symbols.len()
    );
    for d in symbols.iter().take(limit) {
        s.push_str(&format!(
            "- {} `{}` [{}]\n",
            d.symbol.location(),
            d.symbol.label,
            d.symbol.node_type
        ));
    }
    if symbols.len() > limit {
        s.push_str(&format!("… ещё {}\n", symbols.len() - limit));
    }
    s
}
