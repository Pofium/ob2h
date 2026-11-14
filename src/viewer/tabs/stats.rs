//! Таб «Статистика» (Ф70.6): счётчики, размер БД, версия схемы, топы,
//! имена секретов без значений.

use std::path::Path;

use eframe::egui;
use rusqlite::Connection;

use super::{badge, section};
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
            ui.weak(t.no_data);
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                section(ui, t.stats_counters);
                egui::Grid::new("stats_grid")
                    .num_columns(4)
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        counter(ui, t.tab_memory, s.memories);
                        counter(ui, t.stats_memories_deleted, s.memories_deleted);
                        counter(ui, t.tab_graph, s.graph_nodes);
                        counter(ui, t.graph_edges, s.graph_edges);
                        ui.end_row();
                        counter(ui, t.lessons, s.lessons);
                        counter(ui, t.slots, s.slots);
                        counter(ui, t.tab_secrets, s.secrets);
                        counter(ui, "projects", s.projects);
                        ui.end_row();
                        counter(ui, "documents", s.documents);
                        counter(ui, "chunks", s.chunks);
                        ui.weak(t.stats_db_size);
                        ui.label(human_size(s.db_size_bytes));
                        ui.end_row();
                        ui.weak(t.stats_schema);
                        ui.label(s.schema_version.to_string());
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

fn counter(ui: &mut egui::Ui, label: &str, value: i64) {
    ui.weak(label);
    ui.label(egui::RichText::new(value.to_string()).strong());
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