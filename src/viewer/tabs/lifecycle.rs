//! Таб «Уроки · Слоты» (Ф70.5): два подраздела с переключателем.
//! Tombstones уроков скрыты запросом; пустые разделы — честная заглушка «нет данных».

use eframe::egui;
use rusqlite::Connection;

use super::{badge, card_frame, snippet};
use crate::viewer::data::{self, LessonRow, SlotRow};
use crate::viewer::texts::UiTexts;

/// Состояние таба «Уроки · Слоты».
#[derive(Default)]
pub struct LifecycleTab {
    lessons: Vec<LessonRow>,
    slots: Vec<SlotRow>,
    show_slots: bool,
}

impl LifecycleTab {
    /// Перечитывает уроки и слоты.
    pub fn load(&mut self, conn: &Connection) {
        self.lessons = data::load_lessons(conn, 500).unwrap_or_default();
        self.slots = data::load_slots(conn).unwrap_or_default();
    }

    /// Рисует таб.
    pub fn ui(&mut self, ui: &mut egui::Ui, t: &UiTexts) {
        ui.horizontal(|ui| {
            if ui.selectable_label(!self.show_slots, format!("{} ({})", t.lessons, self.lessons.len())).clicked() {
                self.show_slots = false;
            }
            if ui.selectable_label(self.show_slots, format!("{} ({})", t.slots, self.slots.len())).clicked() {
                self.show_slots = true;
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
            ui.weak(t.no_data);
            return;
        }
        for l in &self.lessons {
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
                        badge(ui, tag, egui::Color32::from_rgb(148, 163, 184));
                    }
                });
            });
            ui.add_space(4.0);
        }
    }

    fn slots_ui(&self, ui: &mut egui::Ui, t: &UiTexts) {
        if self.slots.is_empty() {
            ui.weak(t.no_data);
            return;
        }
        for s in &self.slots {
            card_frame(ui, false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&s.label).strong());
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
                    ui.label(egui::RichText::new(snippet(&s.content, 400)).monospace().size(11.5));
                }
            });
            ui.add_space(4.0);
        }
    }
}