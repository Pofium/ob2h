//! Force-layout графа (Ф70.3): чистая математика без egui — тестируется headless.
//!
//! Детерминированная раскладка: стартовые позиции — спираль золотого угла,
//! силы — отталкивание всех пар, пружины рёбер, гравитация к центру, демпфирование.
//! Кап 300 узлов (OB2H_VIEWER_GRAPH_MAX_NODES) держит O(n²) приемлемым.

/// Узел раскладки: позиция/скорость в логических координатах холста.
#[derive(Debug, Clone)]
pub struct LayoutNode {
    pub id: i64,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub degree: usize,
}

const REPULSION: f32 = 2600.0;
const SPRING_K: f32 = 0.05;
const REST_LEN: f32 = 55.0;
const GRAVITY: f32 = 0.012;
const DAMPING: f32 = 0.85;
const MAX_SPEED: f32 = 10.0;
const GOLDEN_ANGLE: f32 = 2.399_963_2;

/// Стартовые позиции: спираль золотого угла вокруг центра (детерминировано).
pub fn initial_positions(ids_degrees: &[(i64, usize)], width: f32, height: f32) -> Vec<LayoutNode> {
    let cx = width / 2.0;
    let cy = height / 2.0;
    ids_degrees
        .iter()
        .enumerate()
        .map(|(i, (id, degree))| {
            let r = 24.0 * (i as f32 + 1.0).sqrt();
            let a = i as f32 * GOLDEN_ANGLE;
            LayoutNode {
                id: *id,
                x: cx + r * a.cos(),
                y: cy + r * a.sin(),
                vx: 0.0,
                vy: 0.0,
                degree: *degree,
            }
        })
        .collect()
}

/// Один шаг симуляции. `edges` — пары индексов в `nodes`.
pub fn step(nodes: &mut [LayoutNode], edges: &[(usize, usize)], width: f32, height: f32) {
    let n = nodes.len();
    if n == 0 {
        return;
    }
    let cx = width / 2.0;
    let cy = height / 2.0;
    let mut fx = vec![0.0f32; n];
    let mut fy = vec![0.0f32; n];

    // Отталкивание всех пар (кулон).
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = nodes[i].x - nodes[j].x;
            let dy = nodes[i].y - nodes[j].y;
            let d2 = (dx * dx + dy * dy).max(25.0);
            let d = d2.sqrt();
            let f = REPULSION / d2;
            let (ux, uy) = (dx / d, dy / d);
            fx[i] += ux * f;
            fy[i] += uy * f;
            fx[j] -= ux * f;
            fy[j] -= uy * f;
        }
    }
    // Пружины рёбер (гук к длине покоя).
    for &(a, b) in edges {
        if a >= n || b >= n || a == b {
            continue;
        }
        let dx = nodes[b].x - nodes[a].x;
        let dy = nodes[b].y - nodes[a].y;
        let d = (dx * dx + dy * dy).sqrt().max(1.0);
        let f = SPRING_K * (d - REST_LEN);
        let (ux, uy) = (dx / d, dy / d);
        fx[a] += ux * f;
        fy[a] += uy * f;
        fx[b] -= ux * f;
        fy[b] -= uy * f;
    }
    // Гравитация к центру + интеграция с демпфированием.
    for i in 0..n {
        fx[i] += (cx - nodes[i].x) * GRAVITY;
        fy[i] += (cy - nodes[i].y) * GRAVITY;
        nodes[i].vx = (nodes[i].vx + fx[i]) * DAMPING;
        nodes[i].vy = (nodes[i].vy + fy[i]) * DAMPING;
        let sp = (nodes[i].vx * nodes[i].vx + nodes[i].vy * nodes[i].vy).sqrt();
        if sp > MAX_SPEED {
            nodes[i].vx *= MAX_SPEED / sp;
            nodes[i].vy *= MAX_SPEED / sp;
        }
        nodes[i].x += nodes[i].vx;
        nodes[i].y += nodes[i].vy;
    }
}

/// Прогоняет симуляцию до устойчивого состояния.
pub fn settle(nodes: &mut [LayoutNode], edges: &[(usize, usize)], iterations: usize, width: f32, height: f32) {
    for _ in 0..iterations {
        step(nodes, edges, width, height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: &LayoutNode, b: &LayoutNode) -> f32 {
        ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
    }

    #[test]
    fn deterministic_layout() {
        let ids = [(1, 2), (2, 1), (3, 1)];
        let mut a = initial_positions(&ids, 800.0, 600.0);
        let mut b = initial_positions(&ids, 800.0, 600.0);
        settle(&mut a, &[(0, 1), (1, 2)], 120, 800.0, 600.0);
        settle(&mut b, &[(0, 1), (1, 2)], 120, 800.0, 600.0);
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.x, y.x);
            assert_eq!(x.y, y.y);
        }
    }

    #[test]
    fn connected_nodes_end_up_closer_than_unconnected() {
        let ids = [(1, 1), (2, 1), (3, 0)];
        let mut nodes = initial_positions(&ids, 800.0, 600.0);
        settle(&mut nodes, &[(0, 1)], 250, 800.0, 600.0);
        assert!(dist(&nodes[0], &nodes[1]) < dist(&nodes[0], &nodes[2]));
        assert!(nodes.iter().all(|n| n.x.is_finite() && n.y.is_finite()));
    }

    #[test]
    fn nodes_do_not_collapse_to_one_point() {
        let ids: Vec<(i64, usize)> = (0..12).map(|i| (i, 1)).collect();
        let mut nodes = initial_positions(&ids, 800.0, 600.0);
        let edges: Vec<(usize, usize)> = (0..11).map(|i| (i, i + 1)).collect();
        settle(&mut nodes, &edges, 250, 800.0, 600.0);
        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                assert!(dist(&nodes[i], &nodes[j]) > 5.0, "узлы {i}/{j} слиплись");
            }
        }
    }
}