//! Таб «Статистика» (Ф70.6): счётчики-плитки, размер БД, версия схемы, топы,
//! имена секретов без значений. UX: дашборд-плитки вместо плоской сетки.

use std::path::Path;

use eframe::egui;
use rusqlite::Connection;

use super::{badge, empty_state, section};
use crate::viewer::data::{self, StatsData};
use crate::viewer::texts::UiTexts;

/// Состояние таба «Статистика».
#[derive(Default)]
pub struct StatsTab {
    stats: Option<StatsData>,
}

impl StatsTab {
    /// Перечитывает статистику.
    pub fn load(&mut self, conn: &Connection, db_path: &Path) {
        self.stats = data::load_stats(conn, db_path).ok();
    }

    /// Рисует таб.
    pub fn ui(&mut self, ui: &mut egui::Ui, t: &UiTexts) {
        let Some(s) = &self.stats else {
            empty_state(ui, "▥", t.no_data, t.empty_stats);
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                section(ui, t.stats_counters);
                // Плитки: 4 в ряд, крупное число + приглушённая подпись.
                let tiles: [(&str, i64); 10] = [
                    (t.tab_memory, s.memories),
                    (t.stats_memories_deleted, s.memories_deleted),
                    (t.tab_graph, s.graph_nodes),
                    (t.graph_edges, s.graph_edges),
                    (t.lessons, s.lessons),
                    (t.slots, s.slots),
                    (t.tab_secrets, s.secrets),
                    (t.stats_projects, s.projects),
                    (t.stats_documents, s.documents),
                    (t.stats_chunks, s.chunks),
                ];
                // Равные колонки: ширина от доступной ширины ряда. Внутри Grid
                // available_width() первой ячейки — ширина её колонки, не ряда.
                let tile_w = ((ui.available_width() - 3.0 * 8.0) / 4.0).max(140.0);
                egui::Grid::new("stats_tiles")
                    .num_columns(4)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        for (i, (label, value)) in tiles.iter().enumerate() {
                            tile(ui, label, &value.to_string(), tile_w);
                            if i % 4 == 3 {
                                ui.end_row();
                            }
                        }
                        tile(ui, t.stats_db_size, &human_size(s.db_size_bytes), tile_w);
                        tile(ui, t.stats_schema, &s.schema_version.to_string(), tile_w);
                        ui.end_row();
                    });

                section(ui, t.stats_top_categories);
                top_bars(ui, &s.top_categories);

                section(ui, t.stats_top_projects);
                top_bars(ui, &s.top_projects);

                section(ui, t.stats_secret_names);
                ui.weak(t.stats_secret_names_note);
                ui.add_space(2.0);
                if s.secret_names.is_empty() {
                    ui.weak(t.no_data);
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for name in &s.secret_names {
                            badge(ui, name, egui::Color32::from_rgb(244, 63, 94));
                        }
                    });
                }
            });
    }
}

/// Плитка-счётчик: карточка, крупное значение, подпись снизу.
/// `outer_w` — внешняя ширина плитки (равные колонки дашборда).
fn tile(ui: &mut egui::Ui, label: &str, value: &str, outer_w: f32) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_width(outer_w - 24.0);
            ui.label(
                egui::RichText::new(value)
                    .text_style(egui::TextStyle::Heading)
                    .size(19.0),
            );
            ui.weak(egui::RichText::new(label).size(11.0));
        });
}

fn top_bars(ui: &mut egui::Ui, rows: &[(String, i64)]) {
    if rows.is_empty() {
        ui.weak("—");
        return;
    }
    let max = rows.iter().map(|(_, c)| *c).max().unwrap_or(1).max(1);
    for (name, count) in rows {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(name).size(12.0));
            ui.add(
                egui::ProgressBar::new(*count as f32 / max as f32)
                    .desired_width(160.0)
                    .text(count.to_string()),
            );
        });
    }
}

/// Человекочитаемый размер.
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}