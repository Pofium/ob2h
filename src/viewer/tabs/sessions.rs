//! Таб «Сессии» (Ф70.4): daily-логи как лента дискретных событий — аналог
//! Replay в agentmemory (бриф): поиск сверху, пары user/assistant карточками.

use std::path::Path;

use eframe::egui;

use super::{badge, card_frame, snippet};
use crate::lifecycle::timeline::SessionEntry;
use crate::viewer::data;
use crate::viewer::texts::UiTexts;

/// Кап отображаемых записей (лента не должна тормозить на больших корпусах).
const DISPLAY_CAP: usize = 300;

/// Состояние таба «Сессии».
#[derive(Default)]
pub struct SessionsTab {
    query: String,
    entries: Vec<SessionEntry>,
}

impl SessionsTab {
    /// Перечитывает daily-логи (read-only, без создания каталогов).
    pub fn load(&mut self, data_dir: &Path) {
        self.entries = data::load_sessions(data_dir);
    }

    /// Рисует ленту сессий (новые сверху).
    pub fn ui(&mut self, ui: &mut egui::Ui, t: &UiTexts) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(t.search)
                    .desired_width(300.0),
            );
            ui.weak(format!("{}: {}", t.tab_sessions, self.entries.len()));
        });
        ui.add_space(4.0);

        if self.entries.is_empty() {
            ui.weak(t.no_data);
            return;
        }

        let q = self.query.trim().to_lowercase();
        let filtered: Vec<&SessionEntry> = self
            .entries
            .iter()
            .rev()
            .filter(|e| {
                q.is_empty()
                    || e.user_text.to_lowercase().contains(&q)
                    || e.assistant_text.to_lowercase().contains(&q)
                    || e.session_id.to_lowercase().contains(&q)
            })
            .take(DISPLAY_CAP)
            .collect();

        if filtered.is_empty() {
            ui.weak(t.no_results);
            return;
        }
        if filtered.len() == DISPLAY_CAP {
            ui.weak(format!("{}: {DISPLAY_CAP}", t.sessions_cap));
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for e in filtered {
                    card_frame(ui, false)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(snippet(&e.timestamp, 19)).strong().size(12.0));
                                if !e.session_id.is_empty() {
                                    badge(ui, &snippet(&e.session_id, 24), egui::Color32::from_rgb(99, 102, 241));
                                }
                                if let Some(author) = &e.author {
                                    badge(ui, author, egui::Color32::from_rgb(16, 185, 129));
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.weak(egui::RichText::new(&e.source_ref).size(10.5));
                                    },
                                );
                            });
                            ui.add_space(3.0);
                            if !e.user_text.trim().is_empty() {
                                ui.horizontal(|ui| {
                                    ui.colored_label(egui::Color32::from_rgb(59, 130, 246), t.sessions_user);
                                    ui.label(egui::RichText::new(snippet(&e.user_text, 400)).size(12.5));
                                });
                            }
                            if !e.assistant_text.trim().is_empty() {
                                ui.horizontal(|ui| {
                                    ui.colored_label(egui::Color32::from_rgb(139, 92, 246), t.sessions_assistant);
                                    ui.label(egui::RichText::new(snippet(&e.assistant_text, 400)).size(12.5));
                                });
                            }
                        });
                    ui.add_space(5.0);
                }
            });
    }
}