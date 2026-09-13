//! Ф40.1 (трек C, PLAN_v1.4): «зоны» — сообщества в кодовом графе проекта.
//!
//! Детекция label propagation (pure-Rust, детерминированно: обход узлов в
//! отсортированном порядке, тай-брейк по минимальному номеру метки) +
//! модулярность Q (стандартная формула) для честной оценки качества разбиения.
//! Это Louvain-lite: без иерархии и фазовых оптимизаций, но устойчиво и без
//! внешних зависимостей; зоны идут в `project_report`.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Zone {
    pub id: usize,
    pub size: usize,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ZonesReport {
    pub modularity_q: f64,
    pub zones: Vec<Zone>,
}

/// Детекция зон по графу проекта (узлы-символы, рёбра любых типов).
pub fn detect_zones(
    conn: &Connection,
    project_id: &str,
    min_size: usize,
) -> rusqlite::Result<ZonesReport> {
    // 1. Узлы
    let mut stmt = conn.prepare(
        "SELECT id, label FROM graph_nodes
         WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')
           AND node_type IN ('Function', 'Method', 'Struct', 'Class', 'Trait', 'Interface', 'Module', 'File')",
    )?;
    let nodes: Vec<(i64, String)> = stmt
        .query_map(params![project_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<_, _>>()?;
    if nodes.is_empty() {
        return Ok(ZonesReport {
            modularity_q: 0.0,
            zones: Vec::new(),
        });
    }

    let idx_of: HashMap<i64, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();

    // 2. Рёбра между узлами проекта
    let mut estmt = conn.prepare(
        "SELECT e.source_id, e.target_id, e.weight FROM graph_edges e
         JOIN graph_nodes s ON s.id = e.source_id
         JOIN graph_nodes t ON t.id = e.target_id
         WHERE e.deleted_at IS NULL AND s.project_id = ?1 AND t.project_id = ?1",
    )?;
    let edges: Vec<(usize, usize, f64)> = estmt
        .query_map(params![project_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })?
        .filter_map(|row| {
            let (s, t, w) = row.ok()?;
            let (si, ti) = (idx_of.get(&s).copied()?, idx_of.get(&t).copied()?);
            if si == ti {
                None
            } else {
                Some((si, ti, w.max(0.0)))
            }
        })
        .collect();

    // 3. Label propagation (детерминированно)
    let n = nodes.len();
    let mut labels: Vec<usize> = (0..n).collect();
    let mut neigh: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for (s, t, w) in &edges {
        neigh[*s].push((*t, *w));
        neigh[*t].push((*s, *w));
    }
    let order: Vec<usize> = (0..n).collect(); // узлы уже в детерминированном порядке БД
    for _round in 0..10 {
        let mut changed = 0;
        for &u in &order {
            // взвешенный голос соседей; тай-брейк — минимальная метка
            let mut votes: HashMap<usize, f64> = HashMap::new();
            for (v, w) in &neigh[u] {
                *votes.entry(labels[*v]).or_default() += w;
            }
            if votes.is_empty() {
                continue;
            }
            let best = votes
                .iter()
                .min_by(|a, b| {
                    b.1.partial_cmp(a.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.0.cmp(b.0))
                })
                .unwrap()
                .0;
            if *best != labels[u] {
                labels[u] = *best;
                changed += 1;
            }
        }
        if changed == 0 {
            break;
        }
    }

    // 4. Модулярность Q = Σ_c [ W_c/m − (D_c/(2m))² ],
    // где W_c — суммарный вес intra-кластерных рёбер (без дублей), D_c — сумма
    // степеней узлов кластера, m — суммарный вес рёбер графа.
    let total_w: f64 = edges.iter().map(|(_, _, w)| w).sum::<f64>();
    let m = total_w; // 2m ≡ Σk_i (каждое ребро даёт 2 к сумме степеней)
    let degree: Vec<f64> = {
        let mut d = vec![0.0; n];
        for (s, t, w) in &edges {
            d[*s] += *w;
            d[*t] += *w;
        }
        d
    };
    let q = if m > 0.0 {
        let mut w_in: HashMap<usize, f64> = HashMap::new();
        for (s, t, w) in &edges {
            if labels[*s] == labels[*t] {
                *w_in.entry(labels[*s]).or_default() += *w;
            }
        }
        let mut sum: f64 = 0.0;
        let mut seen: HashSet<usize> = HashSet::new();
        for l in &labels {
            if seen.insert(*l) {
                let deg: f64 = (0..n).filter(|i| labels[*i] == *l).map(|i| degree[i]).sum();
                let w = w_in.get(l).copied().unwrap_or(0.0);
                let d2m = deg / (2.0 * m);
                sum += w / m - d2m * d2m;
            }
        }
        sum
    } else {
        0.0
    };

    // 5. Зоны
    let mut by_label: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, (_, label)) in nodes.iter().enumerate() {
        by_label.entry(labels[i]).or_default().push(label.clone());
    }
    let mut zones: Vec<Zone> = by_label
        .into_values()
        .filter(|members| members.len() >= min_size)
        .map(|mut members| {
            members.sort();
            Zone {
                id: 0,
                size: members.len(),
                members,
            }
        })
        .collect();
    zones.sort_by(|a, b| b.size.cmp(&a.size).then(a.members.cmp(&b.members)));
    for (i, z) in zones.iter_mut().enumerate() {
        z.id = i + 1;
    }
    Ok(ZonesReport {
        modularity_q: q,
        zones,
    })
}

/// Символы проекта, упомянутые в тексте (Ф40.3, OneKE-lite): God Nodes
/// приоритетно, всего до `limit` лейблов.
pub fn mentioned_symbols(
    conn: &Connection,
    project_id: &str,
    content: &str,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT label, is_god_node FROM graph_nodes
         WHERE project_id = ?1 AND (deleted_at IS NULL OR deleted_at = '')
           AND node_type IN ('Function', 'Method', 'Struct', 'Class', 'Trait', 'Interface')
           AND LENGTH(label) >= 4
         ORDER BY is_god_node DESC, id DESC LIMIT 400",
    )?;
    let rows = stmt
        .query_map(params![project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1).unwrap_or(0)))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out: Vec<String> = Vec::new();
    // сначала God Nodes (сильный сигнал), затем остальные
    let (gods, rest): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(_, god)| *god == 1);
    for (label, _) in gods.into_iter().chain(rest) {
        if out.len() >= limit {
            break;
        }
        if content.contains(&label) && !out.contains(&label) {
            out.push(label);
        }
    }
    Ok(out)
}

/// Markdown-секция зон для `project_report`.
pub fn format_zones(report: &ZonesReport) -> String {
    if report.zones.is_empty() {
        return String::new();
    }
    let mut md = format!(
        "\n## Зоны (communities, label propagation; Q={:.3})\n\n",
        report.modularity_q
    );
    for z in report.zones.iter().take(5) {
        let preview: Vec<&str> = z.members.iter().take(8).map(|s| s.as_str()).collect();
        md.push_str(&format!(
            "- **Зона {}** ({} узл.): {}\n",
            z.id,
            z.size,
            preview.join(", ")
        ));
    }
    md
}
