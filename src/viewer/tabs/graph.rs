//! Таб «Граф» (Ф70.3): force-layout (headless-математика в layout.rs), холст
//! Painter с панорамой/масштабом, цвет рёбер по типу, клик — детали узла.
//! Кап узлов — OB2H_VIEWER_GRAPH_MAX_NODES (дефолт 300, топ по степени).

use eframe::egui;
use rusqlite::Connection;

use super::badge;
use crate::viewer::data::{self, GraphData};
use crate::viewer::layout::{self, LayoutNode};
use crate::viewer::texts::UiTexts;
use crate::viewer::theme;

/// Размер виртуального холста раскладки.
const CANVAS: egui::Vec2 = egui::vec2(900.0, 640.0);
/// Шагов симуляции при смене данных.
const SETTLE_STEPS: usize = 220;

/// Состояние таба «Граф».
pub struct GraphTab {
    graph: GraphData,
    positions: Vec<LayoutNode>,
    /// Рёбра раскладки: (индекс источника, индекс цели, тип связи).
    edge_idx: Vec<(usize, usize, String)>,
    signature: (usize, usize, i64),
    offset: egui::Vec2,
    zoom: f32,
    selected: Option<i64>,
    max_nodes: usize,
}

impl Default for GraphTab {
    fn default() -> Self {
        Self {
            graph: GraphData::default(),
            positions: Vec::new(),
            edge_idx: Vec::new(),
            signature: (0, 0, 0),
            offset: egui::Vec2::ZERO,
            zoom: 1.0,
            selected: None,
            max_nodes: data::graph_max_nodes(),
        }
    }
}

impl GraphTab {
    /// Перечитывает граф; раскладка пересчитывается только при смене данных.
    pub fn load(&mut self, conn: &Connection) {
        let g = data::load_graph(conn, self.max_nodes).unwrap_or_default();
        let sig = (
            g.nodes.len(),
            g.edges.len(),
            g.nodes.iter().map(|n| n.id).sum::<i64>(),
        );
        if sig != self.signature {
            let ids: Vec<(i64, usize)> = g.nodes.iter().map(|n| (n.id, n.degree)).collect();
            let mut pos = layout::initial_positions(&ids, CANVAS.x, CANVAS.y);
            let index: std::collections::HashMap<i64, usize> =
                g.nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
            let edges: Vec<(usize, usize, String)> = g
                .edges
                .iter()
                .filter_map(|e| {
                    Some((*index.get(&e.source)?, *index.get(&e.target)?, e.label.clone()))
                })
                .collect();
            let pairs: Vec<(usize, usize)> = edges.iter().map(|(a, b, _)| (*a, *b)).collect();
            layout::settle(&mut pos, &pairs, SETTLE_STEPS, CANVAS.x, CANVAS.y);
            self.positions = pos;
            self.edge_idx = edges;
            self.signature = sig;
            if self.selected.map(|id| !index.contains_key(&id)).unwrap_or(false) {
                self.selected = None;
            }
        }
        self.graph = g;
    }

    /// Рисует холст и детали выбранного узла.
    pub fn ui(&mut self, ui: &mut egui::Ui, t: &UiTexts) {
        ui.horizontal(|ui| {
            ui.weak(format!(
                "{}: {} / {} · {}: {}",
                t.graph_nodes,
                self.graph.nodes.len(),
                self.graph.total_nodes,
                t.graph_edges,
                self.graph.edges.len()
            ));
            if self.graph.total_nodes > self.graph.nodes.len() {
                ui.weak(format!("({})", t.graph_cap));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(t.graph_hint);
            });
        });
        ui.add_space(2.0);

        if self.graph.nodes.is_empty() {
            ui.weak(t.no_data);
            return;
        }

        let avail = ui.available_size();
        let canvas_h = (avail.y - 96.0).max(180.0);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(avail.x, canvas_h), egui::Sense::click_and_drag());

        // Панорама и масштаб.
        if response.dragged() {
            self.offset += response.drag_delta();
        }
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if response.hovered() && scroll != 0.0 {
            self.zoom = (self.zoom * (1.0 + scroll * 0.0015)).clamp(0.2, 4.0);
        }

        let center = rect.center() + self.offset;
        let to_screen = |p: &LayoutNode| -> egui::Pos2 {
            egui::pos2(
                center.x + (p.x - CANVAS.x / 2.0) * self.zoom,
                center.y + (p.y - CANVAS.y / 2.0) * self.zoom,
            )
        };

        // Клик — выбор ближайшего узла.
        if response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let mut best: Option<(f32, i64)> = None;
                for p in &self.positions {
                    let d = to_screen(p).distance(pointer);
                    if d < 14.0 && best.map(|(bd, _)| d < bd).unwrap_or(true) {
                        best = Some((d, p.id));
                    }
                }
                self.selected = best.map(|(_, id)| id);
            }
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, ui.visuals().extreme_bg_color);

        // Рёбра — цвет по типу связи.
        for (a, b, label) in &self.edge_idx {
            let (pa, pb) = (&self.positions[*a], &self.positions[*b]);
            let rgb = edge_color(label);
            painter.line_segment(
                [to_screen(pa), to_screen(pb)],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]).linear_multiply(0.55)),
            );
        }

        // Узлы — радиус от степени, цвет от типа.
        for (i, p) in self.positions.iter().enumerate() {
            let node = &self.graph.nodes[i];
            let pos = to_screen(p);
            if !rect.expand(30.0).contains(pos) {
                continue;
            }
            let r = (4.0 + (p.degree as f32).sqrt() * 2.2).min(15.0) * self.zoom.max(0.5);
            let rgb = theme::type_color(&node.node_type);
            let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            painter.circle_filled(pos, r, color);
            if node.is_god_node {
                painter.circle_stroke(pos, r + 2.5, egui::Stroke::new(1.5, ui.visuals().strong_text_color()));
            }
            if self.selected == Some(node.id) {
                painter.circle_stroke(pos, r + 4.0, egui::Stroke::new(2.0, color));
            }
            if self.zoom >= 0.75 || self.selected == Some(node.id) {
                painter.text(
                    egui::pos2(pos.x, pos.y + r + 3.0),
                    egui::Align2::CENTER_TOP,
                    super::snippet(&node.label, 28),
                    egui::FontId::proportional(10.5 * self.zoom.min(1.4)),
                    ui.visuals().weak_text_color(),
                );
            }
        }

        // Детали выбранного узла.
        ui.add_space(4.0);
        if let Some(id) = self.selected {
            if let Some(node) = self.graph.nodes.iter().find(|n| n.id == id) {
                ui.horizontal_wrapped(|ui| {
                    let rgb = theme::type_color(&node.node_type);
                    badge(ui, &node.node_type, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                    ui.label(egui::RichText::new(&node.label).strong());
                    ui.weak(format!("{}: {}", t.graph_degree, node.degree));
                    if let Some(p) = &node.project_id {
                        ui.weak(format!("{}: {p}", t.project));
                    }
                });
                if let Some(desc) = &node.description {
                    if !desc.trim().is_empty() {
                        ui.label(egui::RichText::new(super::snippet(desc, 300)).size(12.0));
                    }
                }
                if let Some(file) = &node.file_path {
                    let line = node.line_start.map(|l| format!(":{l}")).unwrap_or_default();
                    ui.weak(format!("{}: {file}{line}", t.graph_file));
                }
            }
        } else {
            ui.weak(t.graph_hint);
        }
    }
}

/// Цвет ребра по типу связи (CALLS/IMPORTS/… — кодовый граф, RELATED — граф знаний).
fn edge_color(label: &str) -> theme::Rgb {
    match label.to_ascii_uppercase().as_str() {
        "CALLS" => [59, 130, 246],
        "IMPORTS" | "USES" => [16, 185, 129],
        "CONTAINS" | "DEFINES" => [139, 92, 246],
        "EXTENDS" | "IMPLEMENTS" => [245, 158, 11],
        "RELATED" | "RELATES_TO" => [148, 163, 184],
        _ => [99, 102, 241],
    }
}