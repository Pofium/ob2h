//! Итеративный Personalized PageRank (Ф29.1 v1.3; переиспользуется памятью — Ф33.1 v1.4).
//!
//! Идея HippoRAG/2 (§8): single-step multi-hop без графовых СУБД — pure Rust поверх
//! списка рёбер (`graph_edges` для графа знаний, `memory_links` для памяти). Модуль не
//! знает, откуда пришли рёбра: на вход — плоские `(from, to, weight)` с целочисленными
//! индексами узлов, на выход — распределение вероятностей по узлам.
//!
//! Инварианты (зафиксированы специей 29.1 и планом 33.1):
//! * damping `d ∈ [0.5, 0.85]` (старт 0.85) — значение клампится, а не отвергается;
//! * не более [`MAX_ITER`] итераций, сходимость по L1-норме (`tol`);
//! * **degree-normalization**: вес ребра делится на суммарный вес исходящих рёбер узла —
//!   hub-защита, иначе PPR «залипает» на записи-хабе со 100+ рёбрами;
//! * висячие узлы (без исходящих рёбер) не теряют массу — она возвращается в
//!   персонализацию (teleport), как в стандартном PPR;
//! * никаких новых зависимостей — только `std` и `serde_json` для разбора весов.

use std::collections::HashMap;

/// Верхняя граница итераций (спека 29.1: «≤20 итераций»).
pub const MAX_ITER: usize = 20;
/// Минимальный damping (спека 29.1: d ∈ 0.5–0.85).
pub const DAMPING_MIN: f64 = 0.5;
/// Максимальный damping (стартовое значение на bench — 0.85).
pub const DAMPING_MAX: f64 = 0.85;
/// Дефолтная сходимость по L1-норме.
pub const DEFAULT_TOL: f64 = 1e-6;

/// Дефолтные веса рёбер по типу (Ф33.1). Формат конфига `OB2H_PPR_WEIGHTS` —
/// JSON-объект `{"kind": weight}`; зафиксировано тестом `default_weights_are_fixed`.
///
/// **Ф54 (PLAN.md, 2026-10-03) — вердикт калибровки: дефолт НЕ меняем.**
/// Замер по кодовому golden-набору (`data/bench/golden_code.jsonl`, 15 кейсов,
/// `ob2h bench --mode code --project …`) показал, что выдача **не зависит от
/// весов кодовых рёбер вовсе**: uniform 0.5, дифференцированные и даже крайние
/// значения (все 5.0 / все 0.05) дают идентичный recall/MRR — цели находятся
/// на этапе сидов (гибридный поиск), PPR-расширение не меняет ранги. Вывод:
/// данных в пользу изменения нет (ADR-13), кодовые рёбра остаются на
/// [`PPR_UNKNOWN_WEIGHT`]. Для осмысленной калибровки нужен multi-hop golden
/// (цель достижима только через расширение) — бэклог. Инструментарий
/// (`--mode code`) и детерминизм сидов сохранены; переопределение —
/// `OB2H_PPR_WEIGHTS`.
pub const DEFAULT_PPR_WEIGHTS: &str =
    r#"{"manual":1.0,"contradicts":0.8,"causes":0.8,"category":0.5,"same_project":0.3}"#;

/// Вес ребра неизвестного типа (Ф33.1).
pub const PPR_UNKNOWN_WEIGHT: f64 = 0.5;

/// Веса рёбер по типу: `kind → weight` (результат разбора `OB2H_PPR_WEIGHTS`, Ф33.1).
pub type PprWeights = HashMap<String, f64>;

/// Ориентированное ребро с весом. Индексы — позиции узлов в [`PprGraph`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PprEdge {
    pub from: usize,
    pub to: usize,
    pub weight: f64,
}

/// Ограничение бонуса популярности (Ф73.2).
pub const CAP_ACCESS: f64 = 3.0;
/// Коэффициент затухания для кодовых узлов (Ф73.2: код стабилен).
pub const LAMBDA_CODE: f64 = 0.0;
/// Дефолтный коэффициент затухания для узлов памяти (per-day, Ф73.2).
pub const DEFAULT_LAMBDA_MEMORY: f64 = 0.015;
/// Минимальный порог grace-периода для cold-start узлов (Ф73.2: N_grace >= 4).
pub const GRACE_MIN_ACCESS: u64 = 4;
/// Множитель штрафа при Negative Feedback / Mute Node (Ф73.2 / Ф73.6).
pub const MUTE_TRUST_FACTOR: f64 = 0.3;
/// Буст +20% к весу сидов p_0 для файлов из `git diff HEAD` (Ф73.4 Active Working Set Boost).
pub const WORKING_SET_BOOST: f64 = 1.20;

/// Вычислить стартовый вес сида `p_0`: `IDF(node) * trust_score` (HippoRAG-style specificity, Ф73.4).
pub fn compute_seed_idf_weight(total_nodes: usize, in_degree: usize, trust_score: f64) -> f64 {
    let n = total_nodes.max(1) as f64;
    let idf = (1.0 + n / (1.0 + in_degree as f64)).ln().max(0.5);
    let trust = trust_score.clamp(0.0, 1.0);
    (idf * (0.1 + 0.9 * trust)).max(1e-6)
}

/// Применить буст Active Working Set (`WORKING_SET_BOOST = 1.20`) к сидам,
/// относящимся к изменённым незакоммиченным файлам (`git diff HEAD`, Ф73.4).
pub fn apply_working_set_boost(
    seeds: &mut [(usize, f64)],
    node_paths: &[Option<&str>],
    dirty_files: &std::collections::HashSet<String>,
    boost: f64,
) {
    if dirty_files.is_empty() || !boost.is_finite() || boost <= 1.0 {
        return;
    }
    for (idx, w) in seeds.iter_mut() {
        if let Some(Some(path)) = node_paths.get(*idx) {
            let norm = path.replace('\\', "/");
            if dirty_files.contains(&norm) {
                *w *= boost;
            }
        }
    }
}

/// Получить множество относительных путей, изменённых относительно `HEAD` (`git diff --name-only HEAD`).
pub fn git_dirty_files(repo_root: &std::path::Path) -> std::collections::HashSet<String> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["diff", "--name-only", "HEAD"])
        .output()
    else {
        return std::collections::HashSet::new();
    };
    if !out.status.success() {
        return std::collections::HashSet::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().replace('\\', "/"))
        .filter(|l| !l.is_empty())
        .collect()
}

/// Компактное разреженное CSR-представление подграфа в памяти с весами рёбер (Ф73.1).
#[derive(Debug, Clone, Default)]
pub struct GraphAdjacency {
    n: usize,
    /// `offsets[u]..offsets[u + 1]` — срез исходящих рёбер узла `u`.
    offsets: Vec<usize>,
    targets: Vec<usize>,
    weights: Vec<f64>,
    out_weight: Vec<f64>,
    in_degree: Vec<usize>,
    superseded: Vec<bool>,
}

impl GraphAdjacency {
    /// Собрать CSR-граф из списка рёбер.
    pub fn from_edges(n: usize, edges: &[PprEdge], superseded: Option<&[bool]>) -> Self {
        let mut counts = vec![0usize; n];
        let mut in_degree = vec![0usize; n];
        let mut out_weight = vec![0.0f64; n];
        let mut valid_edges = Vec::with_capacity(edges.len());
        for &e in edges {
            if e.from >= n || e.to >= n || !e.weight.is_finite() || e.weight <= 0.0 {
                continue;
            }
            counts[e.from] += 1;
            in_degree[e.to] += 1;
            out_weight[e.from] += e.weight;
            valid_edges.push(e);
        }

        let mut offsets = Vec::with_capacity(n + 1);
        offsets.push(0);
        for c in &counts {
            offsets.push(offsets.last().unwrap() + c);
        }

        let total = *offsets.last().unwrap_or(&0);
        let mut targets = vec![0usize; total];
        let mut weights = vec![0.0f64; total];
        let mut cursor = offsets[..n].to_vec();

        for e in valid_edges {
            let pos = cursor[e.from];
            targets[pos] = e.to;
            weights[pos] = e.weight;
            cursor[e.from] += 1;
        }

        let sup = match superseded {
            Some(s) if s.len() == n => s.to_vec(),
            _ => vec![false; n],
        };

        Self {
            n,
            offsets,
            targets,
            weights,
            out_weight,
            in_degree,
            superseded: sup,
        }
    }

    pub fn node_count(&self) -> usize {
        self.n
    }

    pub fn edge_count(&self) -> usize {
        self.targets.len()
    }

    pub fn in_degree(&self, node: usize) -> usize {
        self.in_degree.get(node).copied().unwrap_or(0)
    }

    pub fn out_weight(&self, node: usize) -> f64 {
        self.out_weight.get(node).copied().unwrap_or(0.0)
    }

    pub fn is_superseded(&self, node: usize) -> bool {
        self.superseded.get(node).copied().unwrap_or(false)
    }

    /// Исходящие рёбра узла `(targets_slice, weights_slice)` без аллокаций.
    pub fn neighbors(&self, node: usize) -> (&[usize], &[f64]) {
        if node >= self.n {
            return (&[], &[]);
        }
        let s = self.offsets[node];
        let e = self.offsets[node + 1];
        (&self.targets[s..e], &self.weights[s..e])
    }
}

/// Параметры целевого узла для расчёта веса ребра U-PPR `W(e)` (Ф73.2).
#[derive(Debug, Clone, Copy)]
pub struct UsageEdgeParams {
    /// Базовый вес ребра (из типа связи / уверенности).
    pub w0: f64,
    /// Счётчик обращений целевого узла.
    pub access_count: u64,
    /// Возраст узла в днях.
    pub age_days: f64,
    /// Кодовый ли это узел (`true` → λ_code = 0, `false` → λ_memory > 0).
    pub is_code: bool,
    /// Оценка доверия узла в `[0.0, 1.0]`.
    pub trust_score: f64,
    /// Входящая степень целевого узла (для In-Degree Penalty хабов).
    pub target_in_degree: usize,
}

/// Вычислить вес ребра U-PPR `W(e)` (Ф73.2):
/// - In-Degree Penalty: `w0 / ln(1 + in_degree)` (с защитой `max(1.0)` при `in_degree <= 1`);
/// - Бонус популярности с ограничением `CAP_ACCESS = 3.0` и защитой cold-start (`N_grace >= 4`);
/// - Затухание по возрасту (`λ_code = 0`, `λ_memory = DEFAULT_LAMBDA_MEMORY`), не применяется к cold-start узлам в grace-периоде;
/// - Гарантированный ненулевой вес доверия: `(0.1 + 0.9 * trust_score)`.
pub fn compute_usage_edge_weight(p: UsageEdgeParams) -> f64 {
    let w0 = if p.w0.is_finite() && p.w0 > 0.0 { p.w0 } else { PPR_UNKNOWN_WEIGHT };

    // 1. In-Degree Penalty для структурных хабов
    let in_deg_denom = (1.0 + p.target_in_degree as f64).ln().max(1.0);
    let base_penalized = w0 / in_deg_denom;

    // 2. Cold-start grace period (N_grace >= 4) и ограничение популярности CAP_ACCESS = 3.0
    let is_grace = p.access_count < GRACE_MIN_ACCESS;
    let access_bonus = if is_grace {
        // Cold-start защита: новый узел получает нейтральный/стартовый множитель >= 1.0
        1.0_f64.max((1.0 + (p.access_count as f64).ln_1p() * 0.25).min(CAP_ACCESS))
    } else {
        (1.0 + (p.access_count as f64).ln_1p() * 0.5).min(CAP_ACCESS)
    };

    // 3. Раздельное затухание: код стабилен (λ=0), память затухает (если не в grace)
    let lambda = if p.is_code || is_grace {
        LAMBDA_CODE
    } else {
        DEFAULT_LAMBDA_MEMORY
    };
    let age = p.age_days.max(0.0);
    let decay = (-lambda * age).exp();

    // 4. Гарантированный ненулевой вес доверия (Mute Node бьёт trust_score *= 0.3)
    let trust = p.trust_score.clamp(0.0, 1.0);
    let trust_factor = 0.1 + 0.9 * trust;

    (base_penalized * access_bonus * decay * trust_factor).max(1e-9)
}

/// Персонализированный PageRank поверх [`GraphAdjacency`] (CSR) с исключением
/// `SUPERSEDED` узлов из целевого распределения (Ф73.1, Ф73.5).
pub fn personalized_pagerank_csr(
    adj: &GraphAdjacency,
    seeds: &[(usize, f64)],
    damping: f64,
    max_iter: usize,
    tol: f64,
) -> Vec<(usize, f64)> {
    let n = adj.node_count();
    if n == 0 || seeds.is_empty() {
        return Vec::new();
    }

    let mut seed = vec![0.0f64; n];
    let mut total = 0.0f64;
    for &(node, w) in seeds {
        if node < n && !adj.is_superseded(node) && w.is_finite() && w > 0.0 {
            seed[node] += w;
            total += w;
        }
    }
    if total <= 0.0 {
        return Vec::new();
    }
    for s in &mut seed {
        *s /= total;
    }

    let d = damping.clamp(DAMPING_MIN, DAMPING_MAX);
    let iters = max_iter.clamp(1, MAX_ITER);
    let tol = if tol > 0.0 { tol } else { DEFAULT_TOL };

    let mut p = seed.clone();
    for _ in 0..iters {
        let mut next = vec![0.0f64; n];
        let mut dangling = 0.0f64;
        for (u, &pu) in p.iter().enumerate() {
            if pu == 0.0 {
                continue;
            }
            let (nbrs, wts) = adj.neighbors(u);
            let active_deg: f64 = nbrs
                .iter()
                .zip(wts.iter())
                .filter(|(&v, _)| !adj.is_superseded(v))
                .map(|(_, &w)| w)
                .sum();
            if active_deg <= 0.0 {
                dangling += pu;
                continue;
            }
            for (&v, &w) in nbrs.iter().zip(wts.iter()) {
                if !adj.is_superseded(v) {
                    next[v] += d * pu * (w / active_deg);
                }
            }
        }
        let teleport = (1.0 - d) + d * dangling;
        for (u, &s) in seed.iter().enumerate() {
            if s > 0.0 {
                next[u] += teleport * s;
            }
        }
        let l1: f64 = p.iter().zip(next.iter()).map(|(a, b)| (a - b).abs()).sum();
        p = next;
        if l1 < tol {
            break;
        }
    }

    let mut ranked: Vec<(usize, f64)> = p
        .into_iter()
        .enumerate()
        .filter(|&(u, s)| s > 0.0 && !adj.is_superseded(u))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
}

/// Граф под PPR: `n` узлов, исходящие рёбра в CSR-подобном виде.
#[derive(Debug, Clone, Default)]
pub struct PprGraph {
    n: usize,
    /// `out[u]` — список `(сосед, вес)` исходящих рёбер узла `u`.
    out: Vec<Vec<(usize, f64)>>,
    /// Суммарный вес исходящих рёбер узла — знаменатель degree-normalization.
    out_weight: Vec<f64>,
}

impl PprGraph {
    /// Собрать граф из рёбер. Невалидные рёбра (индекс вне диапазона, не-конечный или
    /// неположительный вес) отбрасываются молча: вызывающий код подмешивает рёбра из
    /// разных источников, и одна битая строка не должна рушить поиск.
    pub fn new(n: usize, edges: &[PprEdge]) -> Self {
        let mut out: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut out_weight = vec![0.0f64; n];
        for e in edges {
            if e.from >= n || e.to >= n || !e.weight.is_finite() || e.weight <= 0.0 {
                continue;
            }
            out[e.from].push((e.to, e.weight));
            out_weight[e.from] += e.weight;
        }
        Self { n, out, out_weight }
    }

    pub fn node_count(&self) -> usize {
        self.n
    }

    pub fn edge_count(&self) -> usize {
        self.out.iter().map(|v| v.len()).sum()
    }

    /// Суммарный вес исходящих рёбер узла (0.0 — висячий узел).
    pub fn out_weight(&self, node: usize) -> f64 {
        self.out_weight.get(node).copied().unwrap_or(0.0)
    }

    /// Исходящие рёбра узла: `(сосед, вес)`.
    pub fn neighbors(&self, node: usize) -> &[(usize, f64)] {
        self.out.get(node).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// Персонализированный PageRank. Возвращает `(узел, score)`, отсортированные по
/// убыванию score (узлы с нулевой массой не попадают в результат).
///
/// * `seeds` — персонализация: пары `(узел, вес)`, нормализуются в распределение.
/// * `damping` — клампится в [`DAMPING_MIN`]..=[`DAMPING_MAX`].
/// * `max_iter` — клампится в `1..=`[`MAX_ITER`].
/// * `tol` — порог L1-сходимости (`<= 0` → [`DEFAULT_TOL`]).
pub fn personalized_pagerank(
    graph: &PprGraph,
    seeds: &[(usize, f64)],
    damping: f64,
    max_iter: usize,
    tol: f64,
) -> Vec<(usize, f64)> {
    let n = graph.n;
    if n == 0 || seeds.is_empty() {
        return Vec::new();
    }

    // Персонализация: суммируем веса сидов одного узла и нормализуем.
    let mut seed = vec![0.0f64; n];
    let mut total = 0.0f64;
    for &(node, w) in seeds {
        if node < n && w.is_finite() && w > 0.0 {
            seed[node] += w;
            total += w;
        }
    }
    if total <= 0.0 {
        return Vec::new();
    }
    for s in seed.iter_mut() {
        *s /= total;
    }

    let d = damping.clamp(DAMPING_MIN, DAMPING_MAX);
    let iters = max_iter.clamp(1, MAX_ITER);
    let tol = if tol > 0.0 { tol } else { DEFAULT_TOL };

    let mut p = seed.clone();
    for _ in 0..iters {
        let mut next = vec![0.0f64; n];
        let mut dangling = 0.0f64;
        for (u, &pu) in p.iter().enumerate() {
            if pu == 0.0 {
                continue;
            }
            let deg = graph.out_weight[u];
            if deg <= 0.0 {
                // Висячий узел: масса уходит в персонализацию (как в стандартном PPR).
                dangling += pu;
                continue;
            }
            // degree-normalization: hub со 100+ рёбрами отдаёт каждому соседу долю,
            // а не полную массу — иначе выдача «залипает» на хабе.
            for &(v, w) in &graph.out[u] {
                next[v] += d * pu * (w / deg);
            }
        }
        let teleport = (1.0 - d) + d * dangling;
        for (u, s) in seed.iter().enumerate() {
            if *s > 0.0 {
                next[u] += teleport * s;
            }
        }

        let l1: f64 = p.iter().zip(next.iter()).map(|(a, b)| (a - b).abs()).sum();
        p = next;
        if l1 < tol {
            break;
        }
    }

    let mut ranked: Vec<(usize, f64)> = p
        .into_iter()
        .enumerate()
        .filter(|(_, s)| *s > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

/// Разобрать веса рёбер из JSON-объекта `{"kind": weight}` (Ф33.1).
/// Битые пары и неположительные веса игнорируются; пустой/невалидный JSON →
/// [`DEFAULT_PPR_WEIGHTS`] — конфиг не должен ломать поиск.
pub fn parse_ppr_weights(json: &str) -> PprWeights {
    let mut out = HashMap::new();
    let parsed: Option<HashMap<String, serde_json::Value>> = serde_json::from_str(json).ok();
    if let Some(map) = parsed {
        for (kind, val) in map {
            if let Some(w) = val.as_f64() {
                if w.is_finite() && w > 0.0 {
                    out.insert(kind.to_lowercase(), w);
                }
            }
        }
    }
    if out.is_empty() {
        return parse_ppr_weights(DEFAULT_PPR_WEIGHTS);
    }
    out
}

/// Вес ребра по типу: неизвестный kind → [`PPR_UNKNOWN_WEIGHT`] (Ф33.1).
pub fn weight_for(weights: &PprWeights, kind: &str) -> f64 {
    weights
        .get(&kind.to_lowercase())
        .copied()
        .unwrap_or(PPR_UNKNOWN_WEIGHT)
}

/// Нормализация сущности при построении рёбер (Ф33.4): lowercase, `ё`→`е`,
/// выравнивание пунктуации, отбрасывание инициалов (однобуквенных токенов) и
/// схлопывание пробелов. Цель — меньше шумовых `entity`/`same_project`-рёбер:
/// «И. Пресняков», «И.Пресняков» и «пресняков» дают одну метку.
pub fn normalize_entity(s: &str) -> String {
    let lowered = s.to_lowercase().replace('ё', "е");
    let mut cleaned = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            cleaned.push(ch);
        } else {
            cleaned.push(' ');
        }
    }
    cleaned
        .split_whitespace()
        // однобуквенные токены — инициалы/мусор; дефисные клеим в один токен
        .filter(|t| t.chars().filter(|c| c.is_alphanumeric()).count() > 1)
        .map(|t| {
            t.chars()
                .filter(|c| *c != '-' && *c != '_')
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: usize, to: usize, weight: f64) -> PprEdge {
        PprEdge { from, to, weight }
    }

    /// 29.4: синтетическая цепочка A→B→C — запрос по A достигает C,
    /// причём C получает строго меньше массы, чем ближний B (multi-hop работает).
    #[test]
    fn chain_reaches_multi_hop() {
        // 0=A, 1=B, 2=C
        let g = PprGraph::new(3, &[edge(0, 1, 1.0), edge(1, 2, 1.0)]);
        let ranked = personalized_pagerank(&g, &[(0, 1.0)], 0.85, MAX_ITER, DEFAULT_TOL);
        let score = |n: usize| {
            ranked
                .iter()
                .find(|(i, _)| *i == n)
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
        };
        assert!(score(2) > 0.0, "C недостижим по цепочке A→B→C: {ranked:?}");
        assert!(
            score(1) > score(2),
            "ближний узел должен быть тяжелее дальнего"
        );
        assert!(score(0) > score(1), "seed — самый тяжёлый узел");
    }

    /// 29.4/33.1: hub-защита — узел со 100+ рёбрами не должен «залипать» выдачу.
    /// Без degree-normalization масса хаба размазалась бы по листьям и затопила
    /// прямой соседний узел.
    #[test]
    fn hub_does_not_flood_ranking() {
        // 0=seed S, 1=прямой сосед A, 2=hub H, 3..=листья H
        let mut edges = vec![edge(0, 1, 1.0), edge(0, 2, 1.0)];
        const LEAVES: usize = 120;
        for i in 0..LEAVES {
            edges.push(edge(2, 3 + i, 1.0));
        }
        let g = PprGraph::new(3 + LEAVES, &edges);
        let ranked = personalized_pagerank(&g, &[(0, 1.0)], 0.85, MAX_ITER, DEFAULT_TOL);
        let score = |n: usize| {
            ranked
                .iter()
                .find(|(i, _)| *i == n)
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
        };

        let direct = score(1);
        let hub = score(2);
        let worst_leaf = (3..3 + LEAVES).map(score).fold(0.0f64, f64::max);
        assert!(direct > 0.0 && hub > 0.0);
        assert!(
            direct > worst_leaf,
            "прямой сосед обязан быть выше любого листа хаба (direct={direct}, leaf={worst_leaf})"
        );
        // масса хаба делится между листьями, а не отдаётся каждому целиком
        let leaf_mass: f64 = (3..3 + LEAVES).map(score).sum();
        assert!(
            leaf_mass < 2.0 * hub,
            "суммарная масса листьев не должна превышать массу хаба более чем вдвое"
        );
    }

    /// Damping клампится в 0.5–0.85, итерации — в 1..=20 (спека 29.1).
    #[test]
    fn damping_and_iterations_are_clamped() {
        let g = PprGraph::new(2, &[edge(0, 1, 1.0)]);
        let low = personalized_pagerank(&g, &[(0, 1.0)], 0.0, 0, DEFAULT_TOL);
        let high = personalized_pagerank(&g, &[(0, 1.0)], 5.0, 999, DEFAULT_TOL);
        // при d=0.5 и d=0.85 распределения различаются, но оба валидны
        assert!(!low.is_empty() && !high.is_empty());
        let low_max = low.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
        let high_max = high.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
        assert!(
            (high_max - low_max).abs() > 1e-6,
            "damping обязан влиять на распределение"
        );
    }

    /// Симметричный граф: равные сиды → равные скоры (санити-тест нормировки).
    #[test]
    fn symmetric_seeds_give_equal_scores() {
        let g = PprGraph::new(2, &[edge(0, 1, 1.0), edge(1, 0, 1.0)]);
        let ranked = personalized_pagerank(&g, &[(0, 1.0), (1, 1.0)], 0.85, MAX_ITER, DEFAULT_TOL);
        assert_eq!(ranked.len(), 2);
        assert!((ranked[0].1 - ranked[1].1).abs() < 1e-9, "{ranked:?}");
    }

    /// Висячие узлы не теряют массу: сумма распределения остаётся ≈1.
    #[test]
    fn mass_is_conserved_with_dangling_node() {
        let g = PprGraph::new(3, &[edge(0, 1, 1.0)]);
        let ranked = personalized_pagerank(&g, &[(0, 1.0)], 0.85, MAX_ITER, DEFAULT_TOL);
        let sum: f64 = ranked.iter().map(|(_, s)| *s).sum();
        assert!((sum - 1.0).abs() < 1e-3, "сумма масс = {sum}");
    }

    /// Пустые сиды/пустой граф — пустой результат, без паники.
    #[test]
    fn empty_inputs_are_safe() {
        let g = PprGraph::new(0, &[]);
        assert!(personalized_pagerank(&g, &[(0, 1.0)], 0.85, MAX_ITER, DEFAULT_TOL).is_empty());
        let g2 = PprGraph::new(2, &[edge(0, 1, 1.0)]);
        assert!(personalized_pagerank(&g2, &[], 0.85, MAX_ITER, DEFAULT_TOL).is_empty());
        assert!(
            personalized_pagerank(&g2, &[(0, 0.0), (1, -1.0)], 0.85, MAX_ITER, DEFAULT_TOL)
                .is_empty()
        );
    }

    /// Битые рёбра отбрасываются, веса читаются из конфига (33.1).
    #[test]
    fn invalid_edges_are_skipped() {
        let g = PprGraph::new(
            2,
            &[
                edge(0, 1, 0.0),
                edge(0, 5, 1.0),
                edge(1, 0, f64::NAN),
                edge(0, 1, 0.7),
            ],
        );
        assert_eq!(g.edge_count(), 1);
        assert!((g.out_weight(0) - 0.7).abs() < 1e-12);
    }

    /// 33.1: дефолтные веса зафиксированы тестом — их изменение = осознанное решение.
    #[test]
    fn default_weights_are_fixed() {
        let w = parse_ppr_weights(DEFAULT_PPR_WEIGHTS);
        assert_eq!(w.len(), 5);
        assert!((w["manual"] - 1.0).abs() < 1e-12);
        assert!((w["contradicts"] - 0.8).abs() < 1e-12);
        assert!((w["causes"] - 0.8).abs() < 1e-12);
        assert!((w["category"] - 0.5).abs() < 1e-12);
        assert!((w["same_project"] - 0.3).abs() < 1e-12);
        assert!((weight_for(&w, "manual") - 1.0).abs() < 1e-12);
        assert!((weight_for(&w, "CALLS") - PPR_UNKNOWN_WEIGHT).abs() < 1e-12);
        assert!((weight_for(&w, "unknown-kind") - PPR_UNKNOWN_WEIGHT).abs() < 1e-12);
        // невалидный конфиг не ломает поиск — откат на дефолт
        assert_eq!(parse_ppr_weights("не json").len(), 5);
        assert_eq!(parse_ppr_weights("").len(), 5);
    }

    /// 33.4: нормализация сущностей — инициалы, регистр, ё/е.
    #[test]
    fn entity_normalization_collapses_forms() {
        assert_eq!(
            normalize_entity("И. Пресняков"),
            normalize_entity("ПРЕСНЯКОВ")
        );
        assert_eq!(
            normalize_entity("И.Пресняков"),
            normalize_entity("и пресняков")
        );
        assert_eq!(normalize_entity("Ёлка"), "елка");
        assert_eq!(normalize_entity("ООО «Ромашка»"), "ооо ромашка");
        // дефисные формы схлопываются в один токен
        assert_eq!(normalize_entity("кое-что"), "коечто");
    }

    #[test]
    fn test_graph_adjacency_csr_and_superseded_exclusion() {
        let edges = vec![edge(0, 1, 1.0), edge(0, 2, 1.0), edge(1, 3, 1.0)];
        let adj = GraphAdjacency::from_edges(4, &edges, Some(&[false, true, false, false]));
        assert_eq!(adj.node_count(), 4);
        assert_eq!(adj.edge_count(), 3);
        assert_eq!(adj.in_degree(1), 1);
        assert!(adj.is_superseded(1));

        let ranked = personalized_pagerank_csr(&adj, &[(0, 1.0)], 0.85, 20, 1e-6);
        assert!(ranked.iter().all(|&(u, _)| u != 1), "SUPERSEDED узел 1 должен быть исключён");
        assert!(ranked.iter().any(|&(u, _)| u == 2));
    }

    #[test]
    fn test_u_ppr_edge_weight_formula_invariants() {
        // 1. CAP_ACCESS = 3.0 ограничивает сверхпопулярные узлы
        let w_10k = compute_usage_edge_weight(UsageEdgeParams {
            w0: 1.0,
            access_count: 10_000,
            age_days: 0.0,
            is_code: true,
            trust_score: 1.0,
            target_in_degree: 1,
        });
        assert!(w_10k <= CAP_ACCESS + 1e-9);

        // 2. Код стабилен (λ=0), память затухает со временем
        let w_code_old = compute_usage_edge_weight(UsageEdgeParams {
            w0: 1.0,
            access_count: 10,
            age_days: 120.0,
            is_code: true,
            trust_score: 1.0,
            target_in_degree: 1,
        });
        let w_mem_old = compute_usage_edge_weight(UsageEdgeParams {
            w0: 1.0,
            access_count: 10,
            age_days: 120.0,
            is_code: false,
            trust_score: 1.0,
            target_in_degree: 1,
        });
        assert!(w_code_old > w_mem_old * 3.0);

        // 3. In-degree penalty подавляет хабы
        let w_leaf = compute_usage_edge_weight(UsageEdgeParams {
            w0: 1.0,
            access_count: 5,
            age_days: 0.0,
            is_code: true,
            trust_score: 1.0,
            target_in_degree: 1,
        });
        let w_hub = compute_usage_edge_weight(UsageEdgeParams {
            w0: 1.0,
            access_count: 5,
            age_days: 0.0,
            is_code: true,
            trust_score: 1.0,
            target_in_degree: 200,
        });
        assert!(w_hub < w_leaf * 0.25);

        // 4. Mute Node (trust_score *= 0.3) резко роняет вес, но сохраняет > 0 (0.1 + 0.9*trust)
        let w_before_mute = compute_usage_edge_weight(UsageEdgeParams {
            w0: 1.0,
            access_count: 5,
            age_days: 0.0,
            is_code: false,
            trust_score: 1.0,
            target_in_degree: 1,
        });
        let w_after_mute = compute_usage_edge_weight(UsageEdgeParams {
            w0: 1.0,
            access_count: 5,
            age_days: 0.0,
            is_code: false,
            trust_score: 1.0 * MUTE_TRUST_FACTOR,
            target_in_degree: 1,
        });
        assert!(w_after_mute < w_before_mute * 0.4);
        assert!(w_after_mute > 0.0);
    }

    #[test]
    fn test_seed_idf_and_active_working_set_boost() {
        // Редкий узел (in_degree=1) получает больший IDF-вес, чем хаб (in_degree=500)
        let w_rare = compute_seed_idf_weight(1000, 1, 1.0);
        let w_hub = compute_seed_idf_weight(1000, 500, 1.0);
        assert!(w_rare > w_hub * 2.0);

        let mut seeds = vec![(0usize, 1.0), (1usize, 1.0)];
        let paths = [Some("src/clean.rs"), Some("src/dirty.rs")];
        let mut dirty = std::collections::HashSet::new();
        dirty.insert("src/dirty.rs".to_string());
        apply_working_set_boost(&mut seeds, &paths, &dirty, WORKING_SET_BOOST);
        assert!((seeds[0].1 - 1.0).abs() < 1e-9);
        assert!((seeds[1].1 - 1.20).abs() < 1e-9);
    }
}
