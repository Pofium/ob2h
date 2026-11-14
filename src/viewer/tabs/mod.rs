//! Табы окна просмотра (feature gui): тонкие оболочки над headless-ядром
//! (data/layout/secrets_ctl). Стилистика — брендбук Omnes: плоские карточки,
//! пилюли-табы с акцентным индикатором, цветные бейджи данных.

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

/// Бейдж-«пилюля» с цветным текстом (тинт-заливка 14%).
pub fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.14))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.0).color(color));
        });
}

/// Секционный заголовок: uppercase, полужирный бренд-гротеск, приглушённый.
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .text_style(egui::TextStyle::Heading)
            .size(11.0)
            .color(ui.visuals().weak_text_color()),
    );
    ui.add_space(3.0);
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
        .stroke(egui::Stroke::new(
            1.0,
            if selected {
                visuals.selection.stroke.color
            } else {
                visuals.widgets.noninteractive.bg_stroke.color
            },
        ))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
}

/// Таб-пилюля: активная — акцентный тинт + подчёркивание, hover — контур.
/// Возвращает клик. `hover_tip` — подсказка (хоткей).
pub fn pill(
    ui: &mut egui::Ui,
    active: bool,
    label: &str,
    accent: egui::Color32,
    hover_tip: &str,
) -> bool {
    // Цвета копируем до painter-вызовов: ui.visuals() держит immutable-заимствование.
    let weak = ui.visuals().weak_text_color();
    let hover_stroke = ui.visuals().widgets.hovered.bg_stroke.color;
    let fill = if active {
        accent.linear_multiply(0.16)
    } else {
        egui::Color32::TRANSPARENT
    };
    let color = if active { accent } else { weak };
    let resp = egui::Frame::new()
        .fill(fill)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).size(13.0).color(color));
        })
        .response
        .interact(egui::Sense::click());
    if active {
        let r = resp.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(r.left() + 8.0, r.bottom() - 2.0),
                egui::pos2(r.right() - 8.0, r.bottom()),
            ),
            1.0,
            accent,
        );
    } else if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect,
            8.0,
            egui::Stroke::new(1.0, hover_stroke),
            egui::StrokeKind::Inside,
        );
    }
    let resp = if hover_tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(hover_tip)
    };
    resp.clicked()
}

/// Пустое состояние по центру: глиф, заголовок, подсказка «что делать».
pub fn empty_state(ui: &mut egui::Ui, glyph: &str, title: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        // Оптический центр: ~пятая часть высоты, а не прижато к верху.
        ui.add_space((ui.available_height() * 0.22).clamp(24.0, 140.0));
        let muted = ui.visuals().weak_text_color();
        ui.label(egui::RichText::new(glyph).size(28.0).color(muted));
        ui.add_space(8.0);
        ui.label(egui::RichText::new(title).size(13.0).color(muted));
        if !hint.is_empty() {
            ui.add_space(3.0);
            ui.weak(egui::RichText::new(hint).size(11.5));
        }
    });
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

/// Цвет категории памяти (data-viz палитра).
pub fn category_color(category: &str) -> egui::Color32 {
    let rgb = match category {
        "general" => [59, 130, 246],
        "recap" => [139, 92, 246],
        "lesson" | "lessons" => [245, 158, 11],
        "user" | "preference" => [16, 185, 129],
        "project" => [6, 182, 212],
        "secret" | "secrets" => [244, 63, 94],
        _ => [154, 163, 178],
    };
    egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// Флаг хоткея из app (Ctrl+F / Esc) — одноразовый, через temp-данные контекста.
pub fn take_hotkey_flag(ui: &egui::Ui, name: &str) -> bool {
    ui.ctx()
        .data_mut(|d| d.remove_temp::<bool>(egui::Id::unique(name)))
        .unwrap_or(false)
}