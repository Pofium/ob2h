//! Таб «Граф» (Ф70.3): force-layout (headless-математика в layout.rs), холст
//! Painter с панорамой/масштабом, цвет рёбер по типу, клик — детали узла.
//! Кап узлов — OB2H_VIEWER_GRAPH_MAX_NODES (дефолт 300, топ по степени).
//! Тематические срезы: «Общая память» (граф знаний, дефолт), проект, «Все темы».
//! UX: тулбар зума (+/−/сброс), легенда типов, двойной клик — центрировать,
//! наведение — кольцо и подпись узла.

use eframe::egui;
use rusqlite::Connection;

use std::time::{Duration, Instant};

use super::{badge, empty_state};
use crate::viewer::data::{self, GraphData, GraphScope};
use crate::viewer::layout::{self, LayoutNode};
use crate::viewer::texts::UiTexts;
use crate::viewer::theme;

/// Размер виртуального холста раскладки.
const CANVAS: egui::Vec2 = egui::vec2(900.0, 640.0);
/// Шагов симуляции при смене данных.
const SETTLE_STEPS: usize = 220;
/// Период перезагрузки графа: корпус большой, 2-секундный refresh окна
/// не должен держать UI-поток в запросах.
const GRAPH_REFRESH: Duration = Duration::from_secs(10);
/// Список проектов для комбо меняется редко — кэш на минуту.
const PROJECTS_REFRESH: Duration = Duration::from_secs(60);

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
    /// Порог степени «хаба»: подписи таких узлов видны при любом зуме.
    hub_degree: usize,
    /// Тематический срез: общая память / проект / всё.
    scope: GraphScope,
    /// Проекты для комбо срезов.
    projects: Vec<String>,
    /// Время последней перезагрузки (троттлинг).
    last_load: Option<Instant>,
    /// Время последнего обновления списка проектов.
    projects_loaded: Option<Instant>,
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
            hub_degree: 0,
            scope: GraphScope::Knowledge,
            projects: Vec::new(),
            last_load: None,
            projects_loaded: None,
        }
    }
}

impl GraphTab {
    /// Перечитывает граф не чаще GRAPH_REFRESH; раскладка — только при смене данных.
    pub fn load(&mut self, conn: &Connection) {
        if self
            .last_load
            .map(|t| t.elapsed() < GRAPH_REFRESH)
            .unwrap_or(false)
        {
            return;
        }
        self.reload(conn);
    }

    /// Немедленная перезагрузка (смена среза темы).
    pub fn reload(&mut self, conn: &Connection) {
        let need_projects = self
            .projects_loaded
            .map(|t| t.elapsed() >= PROJECTS_REFRESH)
            .unwrap_or(true);
        if need_projects {
            self.projects = data::graph_projects(conn).unwrap_or_default();
            self.projects_loaded = Some(Instant::now());
        }
        let g = data::load_graph(conn, self.max_nodes, &self.scope).unwrap_or_default();
        let sig = (
            g.nodes.len(),
            g.edges.len(),
            g.nodes.iter().map(|n| n.id).sum::<i64>(),
        );
        if sig != self.signature {
            // Порог хаба — 24-я по величине степень (узлов меньше — все подписи).
            let mut degrees: Vec<usize> = g.nodes.iter().map(|n| n.degree).collect();
            degrees.sort_unstable_by(|a, b| b.cmp(a));
            self.hub_degree = degrees.get(23).copied().unwrap_or(0);
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
        self.last_load = Some(Instant::now());
    }

    /// Рисует холст и детали выбранного узла.
    pub fn ui(&mut self, ui: &mut egui::Ui, conn: &Connection, t: &UiTexts) {
        if super::take_hotkey_flag(ui, "ob2h_esc") {
            self.selected = None;
        }
        // Тулбар: срез темы, счётчики слева, зум и сброс справа.
        ui.horizontal_wrapped(|ui| {
            let mut scope = self.scope.clone();
            let combo = egui::ComboBox::from_id_salt("graph_scope")
                .selected_text(match &scope {
                    GraphScope::All => t.graph_scope_all,
                    GraphScope::Knowledge => t.graph_scope_knowledge,
                    GraphScope::Project(p) => p.as_str(),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut scope, GraphScope::All, t.graph_scope_all);
                    ui.selectable_value(&mut scope, GraphScope::Knowledge, t.graph_scope_knowledge);
                    for p in &self.projects {
                        ui.selectable_value(&mut scope, GraphScope::Project(p.clone()), p.as_str());
                    }
                });
            combo.response.on_hover_text(t.graph_scope_tip);
            if scope != self.scope {
                // Смена темы: новый расклад, сброс выбора/панорамы/зума.
                self.scope = scope;
                self.selected = None;
                self.offset = egui::Vec2::ZERO;
                self.zoom = 1.0;
                self.reload(conn);
            }
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
                if ui
                    .button(egui::RichText::new("⟲").size(13.0))
                    .on_hover_text(t.graph_reset)
                    .clicked()
                {
                    self.offset = egui::Vec2::ZERO;
                    self.zoom = 1.0;
                }
                if ui
                    .button(egui::RichText::new("+").size(13.0))
                    .on_hover_text(t.graph_zoom)
                    .clicked()
                {
                    self.zoom = (self.zoom * 1.2).min(4.0);
                }
                ui.weak(format!("{}: {:.0}%", t.graph_zoom, self.zoom * 100.0));
                if ui
                    .button(egui::RichText::new("−").size(13.0))
                    .on_hover_text(t.graph_zoom)
                    .clicked()
                {
                    self.zoom = (self.zoom / 1.2).max(0.2);
                }
            });
        });
        ui.add_space(2.0);

        if self.graph.nodes.is_empty() {
            empty_state(ui, "◉", t.no_data, t.empty_graph);
            return;
        }

        // Легенда типов узлов (до 8, по частоте).
        let mut types: Vec<(String, usize)> = Vec::new();
        for n in &self.graph.nodes {
            match types.iter_mut().find(|(k, _)| *k == n.node_type) {
                Some((_, c)) => *c += 1,
                None => types.push((n.node_type.clone(), 1)),
            }
        }
        types.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        ui.horizontal_wrapped(|ui| {
            ui.weak(egui::RichText::new(format!("{}:", t.graph_legend)).size(11.0));
            for (kind, _) in types.iter().take(8) {
                let rgb = theme::type_color(kind);
                let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, color);
                ui.weak(egui::RichText::new(kind).size(11.0));
            }
        });
        ui.add_space(2.0);

        let avail = ui.available_size();
        // Резерв снизу: с выбранным узлом — блок деталей, иначе — строка-подсказка.
        let reserve = if self.selected.is_some() { 110.0 } else { 34.0 };
        let canvas_h = (avail.y - reserve).max(180.0);
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

        // Клик — выбор ближайшего узла; двойной клик — центрировать на нём.
        let nearest = |pointer: egui::Pos2| -> Option<i64> {
            let mut best: Option<(f32, i64)> = None;
            for p in &self.positions {
                let d = to_screen(p).distance(pointer);
                if d < 14.0 && best.map(|(bd, _)| d < bd).unwrap_or(true) {
                    best = Some((d, p.id));
                }
            }
            best.map(|(_, id)| id)
        };
        let hovered = response.hover_pos().and_then(&nearest);
        if response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                self.selected = nearest(pointer);
            }
        }
        if response.double_clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                if let Some(id) = nearest(pointer) {
                    self.selected = Some(id);
                    if let Some(p) = self.positions.iter().find(|p| p.id == id) {
                        self.offset = egui::vec2(
                            -(p.x - CANVAS.x / 2.0) * self.zoom,
                            -(p.y - CANVAS.y / 2.0) * self.zoom,
                        );
                    }
                }
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

        // Узлы — радиус от степени, цвет от типа; кольцо выбора — акцент бренда.
        let accent = ui.visuals().selection.stroke.color;
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
                painter.circle_stroke(pos, r + 2.5, egui::Stroke::new(1.5, ui.visuals().text_color()));
            }
            if self.selected == Some(node.id) {
                painter.circle_stroke(pos, r + 4.0, egui::Stroke::new(2.0, accent));
            } else if hovered == Some(node.id) {
                painter.circle_stroke(pos, r + 3.0, egui::Stroke::new(1.5, ui.visuals().text_color()));
            }
            // Подписи: хабы всегда, остальные — при зуме ≥ 1.5 либо выборе/наведении.
            let focused = self.selected == Some(node.id) || hovered == Some(node.id);
            if self.zoom >= 1.5 || node.degree >= self.hub_degree || focused {
                painter.text(
                    egui::pos2(pos.x, pos.y + r + 3.0),
                    egui::Align2::CENTER_TOP,
                    super::snippet(&node.label, 28),
                    egui::FontId::proportional(10.5 * self.zoom.min(1.4)),
                    if focused {
                        ui.visuals().text_color()
                    } else {
                        ui.visuals().weak_text_color()
                    },
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
                    ui.label(
                        egui::RichText::new(&node.label)
                            .strong()
                            .color(ui.visuals().text_color()),
                    );
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
            ui.weak(t.graph_select_hint);
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
        "RELATED" | "RELATES_TO" => [154, 163, 178],
        _ => [99, 102, 241],
    }
}