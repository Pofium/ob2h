//! Таб «Память» (Ф70.2): FTS-поиск, фильтры, master-detail (список + детали).
//! Выбор хранится по id записи — авто-обновление не подменяет детали.
//! Ctrl+F — фокус поиска, Esc — сброс выбора (хоткеи из app).

use eframe::egui;
use rusqlite::Connection;

use super::{badge, card_frame, category_color, empty_state, section, snippet, take_hotkey_flag};
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
    /// Выбранная запись — по id (не по индексу: строки пересортировываются).
    selected: Option<i64>,
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
        // Выбор живёт по id: пропал из выдачи — сбрасываем детали.
        match self.selected {
            Some(id) if self.rows.iter().any(|r| r.id == id) => {
                self.links = data::memory_links(conn, id).unwrap_or_default();
            }
            _ => {
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
        if take_hotkey_flag(ui, "ob2h_esc") {
            self.selected = None;
            self.links.clear();
        }
        let focus_search = take_hotkey_flag(ui, "ob2h_focus_search");

        ui.horizontal_wrapped(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id_salt("mem_search")
                    .hint_text(t.search)
                    .desired_width(240.0),
            );
            if focus_search {
                resp.request_focus();
            }
            if !self.query.is_empty()
                && ui
                    .button(egui::RichText::new("✕").size(12.0))
                    .on_hover_text(t.empty_search)
                    .clicked()
            {
                self.query.clear();
            }
            filter_combo(ui, "mem_project", t.project, t.all, &self.projects, &mut self.project);
            filter_combo(ui, "mem_category", t.category, t.all, &self.categories, &mut self.category);
            filter_combo(ui, "mem_source", t.source, t.all, &self.sources, &mut self.source);
            ui.add(egui::Slider::new(&mut self.min_trust, 0.0..=1.0).text(t.min_trust));
            ui.weak(format!("{}: {}", t.results, self.rows.len()));
        });
        if self.filter_sig != self.signature() {
            self.load(conn);
        }
        ui.add_space(4.0);

        if self.rows.is_empty() {
            let (title, hint) = if self.query.trim().is_empty() && self.project.is_none()
                && self.category.is_none() && self.source.is_none()
            {
                (t.no_data, t.empty_memory)
            } else {
                (t.no_results, t.empty_search)
            };
            empty_state(ui, "∅", title, hint);
            return;
        }

        // Широкое окно — master-detail рядом; узкое — детали под списком.
        let wide = ui.available_width() >= 900.0;
        if wide {
            let w = ui.available_width();
            // horizontal_top: полная высота колонки (horizontal в egui 0.37
            // резервирует лишь interact_size.y — скролл-список схлопывался).
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(w * 0.55);
                    self.list_ui(ui, conn, t);
                });
                ui.separator();
                ui.vertical(|ui| {
                    self.detail_ui(ui, t);
                });
            });
        } else {
            self.list_ui(ui, conn, t);
            if self.selected.is_some() {
                ui.separator();
                self.detail_ui(ui, t);
            }
        }
    }

    /// Список записей (скролл).
    fn list_ui(&mut self, ui: &mut egui::Ui, conn: &Connection, t: &UiTexts) {
        egui::ScrollArea::vertical()
            .id_salt("mem_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in &self.rows {
                    let selected = self.selected == Some(row.id);
                    let resp = card_frame(ui, selected)
                        .show(ui, |ui| {
                            // Заголовок с truncate: длинный key не наезжает на мету.
                            // .strong() берёт цвет из widgets.active (акцентная заливка
                            // кнопок) — цвет текста темы задаём явно, иначе в light
                            // теме заголовок белый на белом.
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&row.key)
                                        .strong()
                                        .color(ui.visuals().text_color()),
                                )
                                .truncate(),
                            );
                            ui.label(egui::RichText::new(snippet(&row.content, 160)).size(12.0));
                            ui.horizontal(|ui| {
                                badge(ui, &row.category, category_color(&row.category));
                                ui.weak(
                                    egui::RichText::new(format!("{} {:.2}", t.trust, row.trust))
                                        .size(11.0),
                                );
                                ui.weak(
                                    egui::RichText::new(data::fmt_ts(&row.updated_at)).size(11.0),
                                );
                            });
                        })
                        .response
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        self.selected = Some(row.id);
                        self.links = data::memory_links(conn, row.id).unwrap_or_default();
                    }
                    ui.add_space(4.0);
                }
            });
    }

    /// Панель деталей выбранной записи.
    fn detail_ui(&self, ui: &mut egui::Ui, t: &UiTexts) {
        let Some(id) = self.selected else {
            empty_state(ui, "▤", t.details, t.memory_select_hint);
            return;
        };
        let Some(row) = self.rows.iter().find(|r| r.id == id) else { return };
        egui::ScrollArea::vertical()
            .id_salt("mem_detail")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                detail_view(ui, row, &self.links, t);
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
    ui.label(
        egui::RichText::new(&row.key)
            .text_style(egui::TextStyle::Heading)
            .size(14.0),
    );
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
            ui.label(data::fmt_ts(&row.created_at));
            ui.weak(t.updated);
            ui.label(data::fmt_ts(&row.updated_at));
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