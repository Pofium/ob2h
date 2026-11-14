//! Табы окна просмотра (feature gui): тонкие оболочки над headless-ядром
//! (data/layout/secrets_ctl). Стилистика — omnes agent: карточки, цветные
//! бейджи, приглушённые секционные заголовки (бриф Ф70).

pub mod graph;
pub mod lifecycle;
pub mod memory;
pub mod secrets;
pub mod sessions;
pub mod stats;

use eframe::egui;

/// Активный таб окна.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Memory,
    Graph,
    Sessions,
    Lifecycle,
    Stats,
    Secrets,
}

/// Бейдж-«пилюля» с цветным текстом (омнес-стиль).
pub fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.14))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.0).color(color));
        });
}

/// Приглушённый секционный заголовок (uppercase, как в omnes MemoryPanel).
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(11.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
    ui.add_space(2.0);
}

/// Карточка-контейнер строки списка.
pub fn card_frame(ui: &egui::Ui, selected: bool) -> egui::Frame {
    let visuals = ui.visuals();
    egui::Frame::new()
        .fill(if selected {
            visuals.selection.bg_fill
        } else {
            visuals.faint_bg_color
        })
        .stroke(egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
}

/// Обрезка текста до n символов с многоточием.
pub fn snippet(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}

/// Цвет категории памяти (детерминированный из палитры омнес).
pub fn category_color(category: &str) -> egui::Color32 {
    let rgb = match category {
        "general" => [59, 130, 246],
        "recap" => [139, 92, 246],
        "lesson" | "lessons" => [245, 158, 11],
        "user" | "preference" => [16, 185, 129],
        "project" => [6, 182, 212],
        "secret" | "secrets" => [244, 63, 94],
        _ => [148, 163, 184],
    };
    egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}