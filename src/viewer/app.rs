//! Окно просмотра (eframe/egui, feature gui): тонкая оболочка над headless-ядром.
//!
//! Бренд семейства Omnes (brandbook): charcoal/off-white поверхности, оранжевый
//! сигнальный акцент, логотип ob2h в шапке и иконке окна, шрифты Segoe UI/Consolas.
//! Авто-обновление активного таба: 2 с + F5 (Ф70.1). Хоткеи: Ctrl+1…6 — табы,
//! Ctrl+F — поиск, Esc — сброс выбора. Закрытие окна затирает ключ секретов
//! (Drop + явный shutdown после run_native, Ф71.3).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use super::data;
use super::secrets_ctl::SecretsController;
use super::tabs::{self, Tab};
use super::texts::UiTexts;
use super::theme;
use crate::secrets::pin_input::Lang;

/// Период авто-обновления (Ф70.1).
const REFRESH: Duration = Duration::from_secs(2);

/// Состояние окна просмотра.
pub struct ViewerApp {
    data_dir: PathBuf,
    db_path: PathBuf,
    conn: Option<rusqlite::Connection>,
    open_error: Option<String>,
    texts: UiTexts,
    dark: bool,
    tab: Tab,
    last_refresh: Instant,
    /// Локальное время последнего обновления (статус-бар).
    updated_at: String,
    logo: Option<egui::TextureHandle>,
    memory: tabs::memory::MemoryTab,
    graph: tabs::graph::GraphTab,
    sessions: tabs::sessions::SessionsTab,
    lifecycle: tabs::lifecycle::LifecycleTab,
    stats: tabs::stats::StatsTab,
    secrets: Arc<Mutex<SecretsController>>,
    secrets_tab: tabs::secrets::SecretsTab,
}

impl ViewerApp {
    fn new(data_dir: PathBuf, texts: UiTexts, secrets: Arc<Mutex<SecretsController>>) -> Self {
        let db_path = data_dir.join("ob2h.db");
        let (conn, open_error) = match data::open_readonly(&db_path) {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(format!("[Error] {e}"))),
        };
        let mut app = Self {
            data_dir,
            db_path,
            conn,
            open_error,
            texts,
            dark: true,
            tab: Tab::Memory,
            last_refresh: Instant::now(),
            updated_at: now_hms(),
            logo: None,
            memory: Default::default(),
            graph: Default::default(),
            sessions: Default::default(),
            lifecycle: Default::default(),
            stats: Default::default(),
            secrets,
            secrets_tab: Default::default(),
        };
        app.refresh();
        app
    }

    /// Перечитывает данные активного таба (read-only).
    fn refresh(&mut self) {
        self.updated_at = now_hms();
        let Some(conn) = &self.conn else { return };
        match self.tab {
            Tab::Memory => self.memory.load(conn),
            Tab::Graph => self.graph.load(conn),
            Tab::Sessions => self.sessions.load(&self.data_dir),
            Tab::Lifecycle => self.lifecycle.load(conn),
            Tab::Stats => self.stats.load(conn, &self.db_path),
            Tab::Secrets => {
                if let Ok(mut ctl) = self.secrets.lock() {
                    self.secrets_tab.refresh(&mut ctl, conn);
                }
            }
        }
    }

    /// Шапка: логотип, вордмарк, табы-пилюли, переключатель темы.
    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(self.dark);
        let accent = egui::Color32::from_rgb(p.accent[0], p.accent[1], p.accent[2]);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some(tex) = &self.logo {
                ui.add(
                    egui::Image::new(tex)
                        .fit_to_exact_size(egui::vec2(28.0, 28.0))
                        .corner_radius(6.0),
                );
            }
            ui.label(
                egui::RichText::new("ob2h")
                    .text_style(egui::TextStyle::Heading)
                    .size(17.0),
            );
            ui.weak(egui::RichText::new(self.texts.tagline).size(11.0));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
            let tabs = [
                (Tab::Memory, self.texts.tab_memory),
                (Tab::Graph, self.texts.tab_graph),
                (Tab::Sessions, self.texts.tab_sessions),
                (Tab::Lifecycle, self.texts.tab_lifecycle),
                (Tab::Stats, self.texts.tab_stats),
                (Tab::Secrets, self.texts.tab_secrets),
            ];
            for (i, (tab, label)) in tabs.iter().enumerate() {
                if tabs::pill(ui, self.tab == *tab, label, accent, &format!("Ctrl+{}", i + 1)) {
                    self.tab = *tab;
                    self.refresh();
                    self.last_refresh = Instant::now();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (glyph, tip) = if self.dark {
                    ("☀", self.texts.theme_light)
                } else {
                    ("🌙", self.texts.theme_dark)
                };
                if ui
                    .button(egui::RichText::new(glyph).size(14.0))
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.dark = !self.dark;
                    apply_theme(ctx, self.dark);
                }
            });
        });
        ui.add_space(6.0);
    }

    /// Статус-бар: соединение, путь БД, время обновления.
    fn status(&self, ui: &mut egui::Ui) {
        let p = theme::palette(self.dark);
        let c = |rgb: theme::Rgb| egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (color, label) = if self.conn.is_some() {
                (c(p.ok), self.texts.status_ok)
            } else {
                (c(p.error), self.texts.status_err)
            };
            ui.colored_label(color, egui::RichText::new("●").size(9.0));
            ui.weak(egui::RichText::new(label).size(11.0));
            ui.separator();
            ui.weak(egui::RichText::new(self.db_path.display().to_string()).size(11.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(
                    egui::RichText::new(format!(
                        "{} {} · {}",
                        self.texts.status_updated, self.updated_at, self.texts.refresh_hint
                    ))
                    .size(11.0),
                );
            });
        });
        ui.add_space(3.0);
    }
}

/// Локальное время ЧЧ:ММ:СС для статус-бара.
fn now_hms() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Хоткеи окна: Ctrl+1…6 — индекс таба, Ctrl+F — фокус поиска, Esc — сброс.
/// Чистая функция от InputState — покрывается unit-тестом без окна.
fn hotkeys(i: &egui::InputState) -> (Option<usize>, bool, bool) {
    let ctrl = i.modifiers.ctrl;
    let keys = [
        egui::Key::Num1,
        egui::Key::Num2,
        egui::Key::Num3,
        egui::Key::Num4,
        egui::Key::Num5,
        egui::Key::Num6,
    ];
    let idx = keys.iter().position(|k| ctrl && i.key_pressed(*k));
    (idx, ctrl && i.key_pressed(egui::Key::F), i.key_pressed(egui::Key::Escape))
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.logo.is_none() {
            self.logo = super::brand::logo_image()
                .map(|img| ctx.load_texture("ob2h-logo", img, egui::TextureOptions::LINEAR));
        }

        // Хоткеи: Ctrl+1…6 — табы, Ctrl+F — поиск, Esc — сброс выбора.
        let (tab_idx, focus_search, esc) = ctx.input(hotkeys);
        if let Some(i) = tab_idx {
            self.tab = [
                Tab::Memory,
                Tab::Graph,
                Tab::Sessions,
                Tab::Lifecycle,
                Tab::Stats,
                Tab::Secrets,
            ][i];
            self.refresh();
            self.last_refresh = Instant::now();
        }
        if focus_search {
            ctx.data_mut(|d| d.insert_temp(egui::Id::unique("ob2h_focus_search"), true));
        }
        if esc {
            ctx.data_mut(|d| d.insert_temp(egui::Id::unique("ob2h_esc"), true));
        }

        // Авто-обновление 2 с + F5.
        let f5 = ui.input(|i| i.key_pressed(egui::Key::F5));
        if f5 || self.last_refresh.elapsed() >= REFRESH {
            self.refresh();
            self.last_refresh = Instant::now();
        }
        ctx.request_repaint_after(REFRESH);

        egui::Panel::top("ob2h_header").show(ui, |ui| self.header(ui, &ctx));
        egui::Panel::bottom("ob2h_status").show(ui, |ui| self.status(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);
            if let Some(err) = &self.open_error {
                let p = theme::palette(self.dark);
                let errc = egui::Color32::from_rgb(p.error[0], p.error[1], p.error[2]);
                egui::Frame::new()
                    .fill(errc.linear_multiply(0.12))
                    .stroke(egui::Stroke::new(1.0, errc.linear_multiply(0.5)))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.colored_label(errc, err);
                    });
                return;
            }
            let Some(conn) = &self.conn else { return };
            match self.tab {
                Tab::Memory => self.memory.ui(ui, conn, &self.texts),
                Tab::Graph => self.graph.ui(ui, conn, &self.texts),
                Tab::Sessions => self.sessions.ui(ui, &self.texts),
                Tab::Lifecycle => self.lifecycle.ui(ui, &self.texts),
                Tab::Stats => self.stats.ui(ui, &self.texts),
                Tab::Secrets => {
                    if let Ok(mut ctl) = self.secrets.lock() {
                        self.secrets_tab.ui(ui, conn, &mut ctl, &self.texts);
                    }
                }
            }
        });
    }
}

impl Drop for ViewerApp {
    fn drop(&mut self) {
        // Ф71.3: ключ секретов не переживает закрытие окна.
        if let Ok(mut ctl) = self.secrets.lock() {
            ctl.shutdown();
        }
    }
}

/// Применяет палитру бренда к egui-контексту.
fn apply_theme(ctx: &egui::Context, dark: bool) {
    let p = theme::palette(dark);
    let c = |rgb: theme::Rgb| egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = c(p.panel);
    v.window_fill = c(p.panel);
    v.extreme_bg_color = c(p.bg);
    v.faint_bg_color = c(p.card);
    v.override_text_color = Some(c(p.text));
    v.weak_text_color = Some(c(p.muted));
    v.hyperlink_color = c(p.accent);
    v.error_fg_color = c(p.error);
    v.selection.bg_fill = c(p.selection);
    v.selection.stroke = egui::Stroke::new(1.0, c(p.accent));
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, c(p.border));
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, c(p.text));
    v.widgets.inactive.bg_fill = c(p.card);
    v.widgets.inactive.weak_bg_fill = c(p.card);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, c(p.text));
    v.widgets.hovered.bg_fill = c(p.border);
    v.widgets.hovered.weak_bg_fill = c(p.border);
    v.widgets.active.bg_fill = c(p.accent);
    v.widgets.active.weak_bg_fill = c(p.accent);
    // Текст поверх акцентной заливки — контрастный (брендбук).
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, c(p.accent_fg));
    let radius = egui::CornerRadius::same(8);
    v.widgets.noninteractive.corner_radius = radius;
    v.widgets.inactive.corner_radius = radius;
    v.widgets.hovered.corner_radius = radius;
    v.widgets.active.corner_radius = radius;
    v.window_corner_radius = egui::CornerRadius::same(10);
    ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.set_visuals(v);
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.button_padding = egui::vec2(10.0, 5.0);
        });
    }
}

/// Открывает окно просмотра (блокирует до закрытия). Вызывать из главного потока.
pub fn run_window(data_dir: &Path, secrets_ttl_secs: u64) -> anyhow::Result<()> {
    let lang = Lang::resolve();
    let texts = UiTexts::of(lang);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(texts.window_title)
        // 1000×640 pt: при 125–150% DPI влезает в рабочую область ноутбука
        // (1080p) вместе с рамкой — статус-бар не уходит за экран.
        .with_inner_size([1000.0, 640.0])
        .with_min_inner_size([760.0, 480.0]);
    if let Some(icon) = super::brand::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let dir = data_dir.to_path_buf();
    let ctl = Arc::new(Mutex::new(SecretsController::new(
        &data_dir.join("ob2h.db"),
        secrets_ttl_secs,
    )));
    let ctl_for_app = Arc::clone(&ctl);

    // Паника winit (нет дисплея/не главный поток) → ошибка, а не падение процесса
    // ( precedent ADR-23: pin_gui).
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            "ob2h-viewer",
            options,
            Box::new(move |cc: &eframe::CreationContext<'_>| {
                super::brand::install_fonts(&cc.egui_ctx);
                apply_theme(&cc.egui_ctx, true);
                Ok(Box::new(ViewerApp::new(dir, texts, ctl_for_app)) as Box<dyn eframe::App>)
            }),
        )
    }));

    // Закрытие окна: затирание ключа даже если eframe не дропнул App.
    if let Ok(mut c) = ctl.lock() {
        c.shutdown();
    }

    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => anyhow::bail!(
            "[Error] не удалось открыть окно просмотра: {err} \
             [Подсказка] проверьте графическую подсистему; данные доступны через CLI: ob2h stats, ob2h audit"
        ),
        Err(_) => anyhow::bail!(
            "[Error] окно просмотра недоступно (нет графической подсистемы) \
             [Подсказка] используйте CLI: ob2h stats, ob2h audit, ob2h secrets status"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: egui::Key, ctrl: bool) -> egui::RawInput {
        let mods = egui::Modifiers {
            ctrl,
            ..Default::default()
        };
        egui::RawInput {
            // winit шлёт ModifiersChanged отдельно от Key — повторяем порядок.
            events: vec![
                egui::Event::ModifiersChanged(mods),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: mods,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn hotkeys_tabs_search_esc() {
        let ctx = egui::Context::default();
        let probe = |input: egui::RawInput| {
            let mut out = (None, false, false);
            let full = ctx.run_pass(input, |c| out = c.input(hotkeys));
            full.drop_without_applying_deltas();
            out
        };
        let digits = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
        ];
        for (n, key) in digits.iter().enumerate() {
            let (tab, _, _) = probe(press(*key, true));
            assert_eq!(tab, Some(n), "Ctrl+{} должен переключать таб", n + 1);
        }
        // Без Ctrl цифры не трогают табы; F без Ctrl — не поиск.
        let (tab, search, _) = probe(press(egui::Key::Num2, false));
        assert_eq!(tab, None);
        assert!(!search);
        let (_, search, _) = probe(press(egui::Key::F, true));
        assert!(search);
        let (_, _, esc) = probe(press(egui::Key::Escape, false));
        assert!(esc);
    }
}