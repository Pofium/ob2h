//! Окно просмотра (eframe/egui, feature gui): тонкая оболочка над headless-ядром.
//!
//! Тема — omnes agent (тёмная по умолчанию, красный акцент, переключатель
//! dark/light). Авто-обновление активного таба: 2 с + F5 (Ф70.1). Закрытие окна
//! затирает ключ секретов (Drop + явный shutdown после run_native, Ф71.3).

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
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Авто-обновление 2 с + F5.
        let f5 = ui.input(|i| i.key_pressed(egui::Key::F5));
        if f5 || self.last_refresh.elapsed() >= REFRESH {
            self.refresh();
            self.last_refresh = Instant::now();
        }
        ctx.request_repaint_after(REFRESH);

        let accent = {
            let p = theme::palette(self.dark);
            egui::Color32::from_rgb(p.accent[0], p.accent[1], p.accent[2])
        };

        // Верхняя панель: бренд, табы, тема, подсказка обновления.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("ob2h").strong().size(16.0).color(accent));
            ui.separator();
            let tabs = [
                (Tab::Memory, self.texts.tab_memory),
                (Tab::Graph, self.texts.tab_graph),
                (Tab::Sessions, self.texts.tab_sessions),
                (Tab::Lifecycle, self.texts.tab_lifecycle),
                (Tab::Stats, self.texts.tab_stats),
                (Tab::Secrets, self.texts.tab_secrets),
            ];
            for (tab, label) in tabs {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                    self.refresh();
                    self.last_refresh = Instant::now();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.texts.theme).clicked() {
                    self.dark = !self.dark;
                    apply_theme(&ctx, self.dark);
                }
                ui.weak(self.texts.refresh_hint);
            });
        });
        ui.separator();

        if let Some(err) = &self.open_error {
            ui.colored_label(ui.visuals().error_fg_color, err);
            return;
        }
        let Some(conn) = &self.conn else { return };

        match self.tab {
            Tab::Memory => self.memory.ui(ui, conn, &self.texts),
            Tab::Graph => self.graph.ui(ui, &self.texts),
            Tab::Sessions => self.sessions.ui(ui, &self.texts),
            Tab::Lifecycle => self.lifecycle.ui(ui, &self.texts),
            Tab::Stats => self.stats.ui(ui, &self.texts),
            Tab::Secrets => {
                if let Ok(mut ctl) = self.secrets.lock() {
                    self.secrets_tab.ui(ui, conn, &mut ctl, &self.texts);
                }
            }
        }
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

/// Применяет палитру omnes к egui-контексту.
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(texts.window_title)
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([760.0, 480.0]),
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