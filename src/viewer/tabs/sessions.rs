//! Таб «Сессии» (Ф70.4): daily-логи как лента дискретных событий — аналог
//! Replay в agentmemory (бриф): поиск сверху, пары user/assistant карточками.
//! UX: бейджи ролей, моно-время, счётчик выдачи, тултип полного source_ref.

use std::path::Path;

use eframe::egui;

use super::{badge, card_frame, empty_state, snippet, take_hotkey_flag};
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
        let focus_search = take_hotkey_flag(ui, "ob2h_focus_search");
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id_salt("sessions_search")
                    .hint_text(t.search)
                    .desired_width(300.0),
            );
            if focus_search {
                resp.request_focus();
            }
            if !self.query.is_empty()
                && ui.button(egui::RichText::new("✕").size(12.0)).clicked()
            {
                self.query.clear();
            }
        });
        ui.add_space(4.0);

        if self.entries.is_empty() {
            empty_state(ui, "≡", t.no_data, t.empty_sessions);
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

        ui.weak(format!("{}: {}", t.results, filtered.len()));
        if filtered.is_empty() {
            empty_state(ui, "∅", t.no_results, t.empty_search);
            return;
        }
        if filtered.len() == DISPLAY_CAP {
            ui.weak(format!("{}: {DISPLAY_CAP}", t.sessions_cap));
        }
        ui.add_space(2.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for e in filtered {
                    card_frame(ui, false)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(data::fmt_ts(&e.timestamp))
                                        .monospace()
                                        .size(11.5),
                                );
                                if !e.session_id.is_empty() {
                                    badge(ui, &snippet(&e.session_id, 24), egui::Color32::from_rgb(99, 102, 241));
                                }
                                if let Some(author) = &e.author {
                                    badge(ui, author, egui::Color32::from_rgb(16, 185, 129));
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.weak(egui::RichText::new(snippet(&e.source_ref, 40)).size(10.5))
                                            .on_hover_text(&e.source_ref);
                                    },
                                );
                            });
                            ui.add_space(3.0);
                            if !e.user_text.trim().is_empty() {
                                ui.horizontal(|ui| {
                                    badge(ui, t.sessions_user, egui::Color32::from_rgb(59, 130, 246));
                                    // Wrap: длинная реплика переносится, а не режется краем окна.
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(snippet(&e.user_text, 400)).size(12.5),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Wrap),
                                    );
                                });
                            }
                            if !e.assistant_text.trim().is_empty() {
                                ui.horizontal(|ui| {
                                    badge(ui, t.sessions_assistant, egui::Color32::from_rgb(139, 92, 246));
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(snippet(&e.assistant_text, 400)).size(12.5),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Wrap),
                                    );
                                });
                            }
                        });
                    ui.add_space(5.0);
                }
            });
    }
}