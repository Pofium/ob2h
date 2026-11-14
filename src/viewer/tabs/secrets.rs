//! Таб «Секреты» (Ф71): машина состояний not_initialized → locked → unlocked.
//! PIN вводится только здесь (ADR-18); операции — через SecretsController
//! (SecretsService + аудит actor=user/source_tool=viewer). Значения masked,
//! reveal авто-скрывается через 15 с, копирование — осознанное действие.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui;
use rusqlite::Connection;
use zeroize::Zeroizing;

use super::{badge, card_frame, section};
use crate::secrets::service::SecretRecordMeta;
use crate::viewer::secrets_ctl::{SecretsController, SecretsState};
use crate::viewer::texts::UiTexts;

/// Авто-скрытие показанного значения (Ф71.2).
const REVEAL_TTL: Duration = Duration::from_secs(15);

/// Состояние таба «Секреты».
#[derive(Default)]
pub struct SecretsTab {
    pin: String,
    pin_confirm: String,
    init_mode: bool,
    new_name: String,
    new_value: String,
    new_hint: String,
    revealed: HashMap<String, (Zeroizing<String>, Instant)>,
    confirm_delete: Option<String>,
    /// (текст, это ошибка)
    message: Option<(String, bool)>,
    list: Vec<SecretRecordMeta>,
    /// Предыдущее состояние «заперт» — для автофокуса PIN при переходе.
    was_locked: bool,
}

impl SecretsTab {
    /// Периодическое обновление: гасит протухшие reveal, обновляет список.
    pub fn refresh(&mut self, ctl: &mut SecretsController, ro: &Connection) {
        self.revealed.retain(|_, (_, at)| at.elapsed() < REVEAL_TTL);
        if matches!(ctl.state(ro), SecretsState::Unlocked { .. }) {
            self.list = ctl.list().unwrap_or_default();
        } else {
            self.list.clear();
            self.revealed.clear();
        }
    }

    fn ok(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), false));
    }

    fn err(&mut self, e: anyhow::Error) {
        self.message = Some((e.to_string(), true));
    }

    /// Рисует таб по состоянию сейфа.
    pub fn ui(&mut self, ui: &mut egui::Ui, ro: &Connection, ctl: &mut SecretsController, t: &UiTexts) {
        self.revealed.retain(|_, (_, at)| at.elapsed() < REVEAL_TTL);
        if super::take_hotkey_flag(ui, "ob2h_esc") {
            self.confirm_delete = None;
        }
        let state = ctl.state(ro);
        let locked = matches!(state, SecretsState::Locked);
        let focus_pin = locked && !self.was_locked;
        self.was_locked = locked;

        if let Some((msg, is_err)) = &self.message {
            let color = if *is_err { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
            egui::Frame::new()
                .fill(color.linear_multiply(0.10))
                .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.45)))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(color, if *is_err { "⚠" } else { "✓" });
                        ui.colored_label(color, msg);
                    });
                });
            ui.add_space(4.0);
        }

        match state {
            SecretsState::NotInitialized => self.not_initialized_ui(ui, ctl, t),
            SecretsState::Locked => self.locked_ui(ui, ctl, t, focus_pin),
            SecretsState::Unlocked { ttl_left_sec } => self.unlocked_ui(ui, ctl, t, ttl_left_sec),
        }
    }

    fn not_initialized_ui(&mut self, ui: &mut egui::Ui, ctl: &mut SecretsController, t: &UiTexts) {
        ui.label(egui::RichText::new(t.secrets_intro).size(12.5));
        ui.add_space(6.0);
        if !self.init_mode {
            if ui.button(t.secrets_start).clicked() {
                self.init_mode = true;
                self.message = None;
            }
            return;
        }
        egui::Grid::new("secrets_init_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.weak(t.secrets_pin);
            let resp_pin = ui.add(egui::TextEdit::singleline(&mut self.pin).password(true).desired_width(140.0));
            if self.init_mode && self.pin.is_empty() && self.pin_confirm.is_empty() {
                resp_pin.request_focus();
            }
            ui.end_row();
            ui.weak(t.secrets_pin_confirm);
            let resp = ui.add(egui::TextEdit::singleline(&mut self.pin_confirm).password(true).desired_width(140.0));
            ui.end_row();
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button(t.secrets_init_btn).clicked() || enter {
                match ctl.init(&self.pin, &self.pin_confirm) {
                    Ok(()) => {
                        // Новый сейф сразу разблокирован тем же PIN — список пуст.
                        let pin = std::mem::take(&mut self.pin);
                        self.pin_confirm.clear();
                        self.init_mode = false;
                        let _ = ctl.unlock(&pin);
                        self.list = ctl.list().unwrap_or_default();
                        self.message = None;
                    }
                    Err(e) => self.err(e),
                }
            }
        });
    }

    fn locked_ui(&mut self, ui: &mut egui::Ui, ctl: &mut SecretsController, t: &UiTexts, focus_pin: bool) {
        ui.label(egui::RichText::new(t.secrets_intro).size(12.5));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.weak(t.secrets_pin);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.pin)
                    .password(true)
                    .desired_width(140.0),
            );
            if focus_pin {
                resp.request_focus();
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button(t.secrets_unlock).clicked() || enter {
                let pin = std::mem::take(&mut self.pin);
                match ctl.unlock(&pin) {
                    Ok(()) => {
                        self.list = ctl.list().unwrap_or_default();
                        self.message = None;
                    }
                    Err(e) => self.err(e),
                }
            }
        });
    }

    fn unlocked_ui(
        &mut self,
        ui: &mut egui::Ui,
        ctl: &mut SecretsController,
        t: &UiTexts,
        ttl_left_sec: u64,
    ) {
        ui.horizontal(|ui| {
            // TTL < 60 с — сигнальный оранжевый, иначе зелёный.
            let ttl_color = if ttl_left_sec < 60 {
                ui.visuals().selection.stroke.color
            } else {
                egui::Color32::from_rgb(16, 185, 129)
            };
            badge(ui, &format!("{} {}:{:02}", t.secrets_ttl, ttl_left_sec / 60, ttl_left_sec % 60), ttl_color);
            if ui.button(t.secrets_lock).clicked() {
                ctl.lock();
                self.list.clear();
                self.revealed.clear();
                self.message = None;
            }
        });
        ui.add_space(4.0);

        // Форма добавления.
        section(ui, t.secrets_add);
        egui::Grid::new("secrets_add_grid").num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
            ui.weak(t.secrets_name);
            ui.add(egui::TextEdit::singleline(&mut self.new_name).desired_width(220.0));
            ui.end_row();
            ui.weak(t.secrets_value);
            ui.add(egui::TextEdit::singleline(&mut self.new_value).password(true).desired_width(220.0));
            ui.end_row();
            ui.weak(t.secrets_hint);
            ui.add(egui::TextEdit::singleline(&mut self.new_hint).desired_width(220.0));
            ui.end_row();
        });
        if ui.button(t.secrets_save).clicked() {
            let hint = if self.new_hint.trim().is_empty() { None } else { Some(self.new_hint.as_str()) };
            match ctl.set(self.new_name.trim(), &self.new_value, hint) {
                Ok(()) => {
                    self.new_name.clear();
                    self.new_value.clear();
                    self.new_hint.clear();
                    self.list = ctl.list().unwrap_or_default();
                    self.ok(t.secrets_save);
                }
                Err(e) => self.err(e),
            }
        }
        ui.add_space(6.0);

        // Список секретов.
        section(ui, t.tab_secrets);
        if self.list.is_empty() {
            ui.weak(t.secrets_empty);
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Копия имён, чтобы не держать заимствование self.list в цикле.
                let names: Vec<String> = self.list.iter().map(|m| m.name.clone()).collect();
                for name in names {
                    let Some(meta) = self.list.iter().find(|m| m.name == name) else { continue };
                    // Копии полей, чтобы замыкание карточки не держало заимствование self.list.
                    let hint = meta.hint.clone();
                    let access_count = meta.access_count;
                    card_frame(ui, false).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&name)
                                    .strong()
                                    .color(ui.visuals().text_color()),
                            );
                            if let Some(hint) = &hint {
                                ui.weak(hint);
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.weak(format!("{}: {}", t.access_count, access_count));
                            });
                        });
                        // Значение: masked •••• либо reveal с авто-скрытием.
                        let revealed = self.revealed.get(&name).map(|(v, _)| v.as_str().to_string());
                        ui.horizontal(|ui| {
                            ui.weak(t.secrets_value);
                            match &revealed {
                                Some(v) => {
                                    ui.label(egui::RichText::new(v).monospace());
                                    ui.weak(format!("({})", t.secrets_auto_hide));
                                }
                                None => {
                                    ui.label(egui::RichText::new("••••••••").monospace());
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            if revealed.is_some() {
                                if ui.button(t.secrets_hide).clicked() {
                                    self.revealed.remove(&name);
                                }
                            } else if ui.button(t.secrets_reveal).clicked() {
                                match ctl.reveal(&name) {
                                    Ok(v) => {
                                        self.revealed.insert(name.clone(), (v, Instant::now()));
                                        self.ok(t.secrets_auto_hide);
                                    }
                                    Err(e) => self.err(e),
                                }
                            }
                            if ui.button(t.secrets_copy).on_hover_text(t.secrets_clipboard_warn).clicked() {
                                match ctl.reveal(&name) {
                                    Ok(v) => {
                                        ui.ctx().copy_text(v.as_str().to_string());
                                        self.ok(format!("{} — {}", t.secrets_copied, t.secrets_clipboard_warn));
                                    }
                                    Err(e) => self.err(e),
                                }
                            }
                            if self.confirm_delete.as_deref() == Some(name.as_str()) {
                                ui.colored_label(ui.visuals().error_fg_color, t.secrets_confirm_delete);
                                let danger = ui.visuals().error_fg_color;
                                if ui
                                    .add(
                                        egui::Button::new(egui::RichText::new(t.secrets_delete).color(danger))
                                            .fill(danger.linear_multiply(0.15)),
                                    )
                                    .clicked()
                                {
                                    match ctl.delete(&name) {
                                        Ok(_) => {
                                            self.revealed.remove(&name);
                                            self.confirm_delete = None;
                                            self.list = ctl.list().unwrap_or_default();
                                        }
                                        Err(e) => self.err(e),
                                    }
                                }
                                if ui.button(t.secrets_cancel).clicked() {
                                    self.confirm_delete = None;
                                }
                            } else if ui.button(t.secrets_delete).clicked() {
                                self.confirm_delete = Some(name.clone());
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
    }
}