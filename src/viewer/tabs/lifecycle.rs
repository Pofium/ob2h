//! Таб «Уроки · Слоты» (Ф70.5): процедурный опыт и рабочий контекст агента.
//! Tombstones уроков скрыты запросом; пустые разделы — честный empty state.
//! UX: пилюли-подтабы, поиск по урокам, разворот полного содержимого слота.

use std::collections::HashSet;

use eframe::egui;
use rusqlite::Connection;

use super::{badge, card_frame, empty_state, pill, snippet, take_hotkey_flag};
use crate::viewer::data::{self, LessonRow, SlotRow};
use crate::viewer::texts::UiTexts;
use crate::viewer::theme;

/// Состояние таба «Уроки · Слоты».
#[derive(Default)]
pub struct LifecycleTab {
    lessons: Vec<LessonRow>,
    slots: Vec<SlotRow>,
    show_slots: bool,
    query: String,
    /// Развёрнутые слоты (по label): полное содержимое вместо сниппета.
    expanded: HashSet<String>,
}

impl LifecycleTab {
    /// Перечитывает уроки и слоты.
    pub fn load(&mut self, conn: &Connection) {
        self.lessons = data::load_lessons(conn, 500).unwrap_or_default();
        self.slots = data::load_slots(conn).unwrap_or_default();
    }

    /// Рисует таб.
    pub fn ui(&mut self, ui: &mut egui::Ui, t: &UiTexts) {
        let p = theme::palette(ui.visuals().dark_mode);
        let accent = egui::Color32::from_rgb(p.accent[0], p.accent[1], p.accent[2]);
        ui.horizontal(|ui| {
            if pill(
                ui,
                !self.show_slots,
                &format!("{} ({})", t.lessons, self.lessons.len()),
                accent,
                "",
            ) {
                self.show_slots = false;
            }
            if pill(
                ui,
                self.show_slots,
                &format!("{} ({})", t.slots, self.slots.len()),
                accent,
                "",
            ) {
                self.show_slots = true;
            }
            if !self.show_slots {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id_salt("lessons_search")
                        .hint_text(t.lessons_search)
                        .desired_width(220.0),
                );
                if take_hotkey_flag(ui, "ob2h_focus_search") {
                    resp.request_focus();
                }
                if !self.query.is_empty()
                    && ui.button(egui::RichText::new("✕").size(12.0)).clicked()
                {
                    self.query.clear();
                }
            }
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.show_slots {
                    self.slots_ui(ui, t);
                } else {
                    self.lessons_ui(ui, t);
                }
            });
    }

    fn lessons_ui(&self, ui: &mut egui::Ui, t: &UiTexts) {
        if self.lessons.is_empty() {
            empty_state(ui, "✎", t.no_data, t.empty_lessons);
            return;
        }
        let q = self.query.trim().to_lowercase();
        let shown: Vec<&LessonRow> = self
            .lessons
            .iter()
            .filter(|l| {
                q.is_empty()
                    || l.content.to_lowercase().contains(&q)
                    || l.context.to_lowercase().contains(&q)
                    || l.tags.iter().any(|tag| tag.to_lowercase().contains(&q))
            })
            .collect();
        if shown.is_empty() {
            empty_state(ui, "∅", t.no_results, t.empty_search);
            return;
        }
        for l in shown {
            card_frame(ui, false).show(ui, |ui| {
                ui.label(egui::RichText::new(snippet(&l.content, 300)).size(12.5));
                if !l.context.trim().is_empty() {
                    ui.weak(egui::RichText::new(snippet(&l.context, 160)).size(11.5));
                }
                ui.add_space(3.0);
                ui.horizontal_wrapped(|ui| {
                    ui.weak(t.confidence);
                    ui.add(
                        egui::ProgressBar::new(l.confidence.clamp(0.0, 1.0) as f32)
                            .desired_width(90.0)
                            .text(format!("{:.2}", l.confidence)),
                    );
                    badge(ui, &l.source, egui::Color32::from_rgb(245, 158, 11));
                    if let Some(p) = &l.project {
                        badge(ui, p, egui::Color32::from_rgb(6, 182, 212));
                    }
                    ui.weak(format!("{}: {}", t.reinforcements, l.reinforcements));
                    for tag in l.tags.iter().take(5) {
                        badge(ui, tag, egui::Color32::from_rgb(154, 163, 178));
                    }
                });
            });
            ui.add_space(4.0);
        }
    }

    fn slots_ui(&mut self, ui: &mut egui::Ui, t: &UiTexts) {
        if self.slots.is_empty() {
            empty_state(ui, "▤", t.no_data, t.empty_slots);
            return;
        }
        // Копия имён/полей, чтобы замыкание карточки не держало заимствование self.slots.
        let slots: Vec<SlotRow> = self.slots.clone();
        for s in slots {
            let expanded = self.expanded.contains(&s.label);
            card_frame(ui, false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&s.label)
                            .strong()
                            .color(ui.visuals().text_color()),
                    );
                    if s.project.is_empty() {
                        badge(ui, t.slot_global, egui::Color32::from_rgb(139, 92, 246));
                    } else {
                        badge(ui, &s.project, egui::Color32::from_rgb(6, 182, 212));
                    }
                    if s.pinned {
                        badge(ui, t.slot_pinned, egui::Color32::from_rgb(16, 185, 129));
                    }
                    if s.read_only {
                        badge(ui, t.slot_read_only, egui::Color32::from_rgb(244, 63, 94));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let used = s.content.chars().count();
                        ui.weak(format!("{}: {}/{}", t.slot_size, used, s.size_limit));
                    });
                });
                if !s.description.trim().is_empty() {
                    ui.weak(egui::RichText::new(&s.description).size(11.5));
                }
                if !s.content.trim().is_empty() {
                    ui.add_space(3.0);
                    let text = if expanded {
                        s.content.clone()
                    } else {
                        snippet(&s.content, 400)
                    };
                    ui.label(egui::RichText::new(text).monospace().size(11.5));
                    if s.content.chars().count() > 400 {
                        let label = if expanded { t.slot_collapse } else { t.slot_expand };
                        if ui.small_button(label).clicked() {
                            if expanded {
                                self.expanded.remove(&s.label);
                            } else {
                                self.expanded.insert(s.label.clone());
                            }
                        }
                    }
                }
            });
            ui.add_space(4.0);
        }
    }
}