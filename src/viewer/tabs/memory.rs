//! Таб «Память» (Ф70.2): FTS-поиск, фильтры, детали с метой и связями.
//! Стилистика agentmemory: поиск сверху, компактные строки, детали по клику.

use eframe::egui;
use rusqlite::Connection;

use super::{badge, card_frame, category_color, section, snippet};
use crate::viewer::data::{self, MemoryFilters, MemoryLinkRow, MemoryRow};
use crate::viewer::texts::UiTexts;

/// Состояние таба «Память».
#[derive(Default)]
pub struct MemoryTab {
    query: String,
    project: Option<String>,
    category: Option<String>,
    source: Option<String>,
    min_trust: f32,
    rows: Vec<MemoryRow>,
    selected: Option<usize>,
    links: Vec<MemoryLinkRow>,
    projects: Vec<String>,
    categories: Vec<String>,
    sources: Vec<String>,
    filter_sig: String,
}

impl MemoryTab {
    /// Перечитывает данные из read-only соединения.
    pub fn load(&mut self, conn: &Connection) {
        self.projects = data::distinct_memory_values(conn, "project_id").unwrap_or_default();
        self.categories = data::distinct_memory_values(conn, "category").unwrap_or_default();
        self.sources = data::distinct_memory_values(conn, "source").unwrap_or_default();
        let f = MemoryFilters {
            query: &self.query,
            project: self.project.as_deref(),
            category: self.category.as_deref(),
            source: self.source.as_deref(),
            min_trust: self.min_trust as f64,
            limit: 200,
        };
        self.rows = data::search_memories(conn, &f).unwrap_or_default();
        if let Some(sel) = self.selected {
            if sel >= self.rows.len() {
                self.selected = None;
                self.links.clear();
            }
        }
        self.filter_sig = self.signature();
    }

    fn signature(&self) -> String {
        format!(
            "{}|{:?}|{:?}|{:?}|{:.2}",
            self.query, self.project, self.category, self.source, self.min_trust
        )
    }

    /// Рисует таб; при смене фильтров перечитывает данные сразу.
    pub fn ui(&mut self, ui: &mut egui::Ui, conn: &Connection, t: &UiTexts) {
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(t.search)
                    .desired_width(240.0),
            );
            filter_combo(ui, "mem_project", t.project, t.all, &self.projects, &mut self.project);
            filter_combo(ui, "mem_category", t.category, t.all, &self.categories, &mut self.category);
            filter_combo(ui, "mem_source", t.source, t.all, &self.sources, &mut self.source);
            ui.add(egui::Slider::new(&mut self.min_trust, 0.0..=1.0).text(t.min_trust));
        });
        if self.filter_sig != self.signature() {
            self.load(conn);
        }
        ui.add_space(4.0);

        if self.rows.is_empty() {
            ui.weak(if self.query.trim().is_empty() { t.no_data } else { t.no_results });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, row) in self.rows.iter().enumerate() {
                    let selected = self.selected == Some(i);
                    let resp = card_frame(ui, selected)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&row.key).strong());
                                badge(ui, &row.category, category_color(&row.category));
                                ui.weak(format!("{} {:.2}", t.trust, row.trust));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.weak(snippet(&row.updated_at, 10));
                                    },
                                );
                            });
                            ui.label(egui::RichText::new(snippet(&row.content, 160)).size(12.0));
                        })
                        .response
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        self.selected = Some(i);
                        self.links = data::memory_links(conn, row.id).unwrap_or_default();
                    }
                    ui.add_space(4.0);
                }

                if let Some(sel) = self.selected {
                    if let Some(row) = self.rows.get(sel) {
                        ui.separator();
                        section(ui, t.details);
                        detail_view(ui, row, &self.links, t);
                    }
                }
            });
    }
}

/// ComboBox фильтра со значением «все» (None).
fn filter_combo(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    all: &str,
    values: &[String],
    slot: &mut Option<String>,
) {
    let selected = slot.clone().unwrap_or_else(|| all.to_string());
    egui::ComboBox::from_id_salt(id)
        .selected_text(format!("{label}: {selected}"))
        .show_ui(ui, |ui| {
            ui.selectable_value(slot, None, all);
            for v in values {
                ui.selectable_value(slot, Some(v.clone()), v);
            }
        });
}

/// Детали записи: содержимое, поля, мета, связи.
fn detail_view(ui: &mut egui::Ui, row: &MemoryRow, links: &[MemoryLinkRow], t: &UiTexts) {
    ui.label(egui::RichText::new(&row.key).strong().size(14.0));
    ui.add_space(2.0);
    ui.label(&row.content);
    ui.add_space(4.0);
    egui::Grid::new("mem_detail_grid")
        .num_columns(4)
        .spacing([14.0, 3.0])
        .show(ui, |ui| {
            ui.weak(t.category);
            ui.label(&row.category);
            ui.weak(t.importance);
            ui.label(format!("{:.2}", row.importance));
            ui.end_row();
            ui.weak(t.source);
            ui.label(&row.source);
            ui.weak(t.trust);
            ui.label(format!("{:.2}", row.trust));
            ui.end_row();
            ui.weak(t.project);
            ui.label(row.project_id.as_deref().unwrap_or("—"));
            ui.weak(t.access_count);
            ui.label(row.access_count.to_string());
            ui.end_row();
            ui.weak(t.created);
            ui.label(snippet(&row.created_at, 19));
            ui.weak(t.updated);
            ui.label(snippet(&row.updated_at, 19));
            ui.end_row();
        });

    if let Some(meta) = &row.meta {
        if !meta.trim().is_empty() {
            section(ui, t.memory_meta);
            let pretty = serde_json::from_str::<serde_json::Value>(meta)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| meta.clone());
            egui::ScrollArea::vertical()
                .id_salt("mem_meta_scroll")
                .max_height(120.0)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(pretty).monospace().size(11.5));
                });
        }
    }

    if !links.is_empty() {
        section(ui, t.memory_links);
        for l in links {
            ui.horizontal(|ui| {
                badge(ui, &l.kind, egui::Color32::from_rgb(99, 102, 241));
                ui.label(&l.to_key);
                ui.weak(format!("w={:.2}", l.weight));
            });
        }
    }
}