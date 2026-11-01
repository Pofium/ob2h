//! Компактное окно ввода 4-значного PIN-кода (egui/eframe).
//!
//! PIN вводится **только локально** в процессе ob2h (ADR-18): окно не участвует в
//! обмене с MCP/LLM/чатом и живёт вне пайплайна агента — harness лишь запускает
//! CLI-команду модуля секретов, которая открывает это окно.
//!
//! Модуль собирается только при feature `gui`. Если окно недоступно (нет
//! графической подсистемы, headless-сессия, установлен `OB2H_PIN_GUI=0`),
//! [`ask_pin`] возвращает ошибку — вызывающий код откатывается на терминал.

use std::sync::{Arc, Mutex};

use eframe::egui;

use super::pin_input::{gui_disabled_by_env, Lang, PinBuffer, PinPurpose, Texts, PIN_LEN};

/// Ширина окна в логических точках.
const WIN_W: f32 = 380.0;
/// Высота окна в логических точках.
const WIN_H: f32 = 240.0;
/// Сторона квадратного поля ввода.
const FIELD: f32 = 62.0;
/// Промежуток между полями.
const GAP: f32 = 14.0;
/// Отступ полей сверху (от верха окна).
const FIELDS_TOP: f32 = 92.0;
/// Радиус скругления полей.
const CORNER: f32 = 10.0;

/// Слот результата: `Some(value)` — окно приняло решение (`None` внутри = отмена).
type Slot = Arc<Mutex<Option<Option<String>>>>;

/// Сценарий окна: одиночный PIN или «новый PIN + подтверждение».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Одно значение.
    Single,
    /// Новый PIN, затем его повтор (сверка внутри окна).
    NewWithConfirm,
}

/// Состояние окна ввода PIN.
struct PinApp {
    texts: Texts,
    step: Step,
    purpose: PinPurpose,
    confirm_purpose: PinPurpose,
    /// Идёт ввод подтверждения (только для [`Step::NewWithConfirm`]).
    confirming: bool,
    buffer: PinBuffer,
    /// Первый ввод, ожидающий подтверждения.
    first: Option<String>,
    /// Показать сообщение о несовпадении.
    mismatch: bool,
    /// Решение принято, окно закрывается.
    done: bool,
    slot: Slot,
}

impl PinApp {
    /// Создаёт состояние окна.
    fn new(
        lang: Lang,
        step: Step,
        purpose: PinPurpose,
        confirm_purpose: PinPurpose,
        slot: Slot,
    ) -> Self {
        Self {
            texts: Texts::of(lang),
            step,
            purpose,
            confirm_purpose,
            confirming: false,
            buffer: PinBuffer::new(),
            first: None,
            mismatch: false,
            done: false,
            slot,
        }
    }

    /// Назначение текущего шага ввода.
    fn current_purpose(&self) -> PinPurpose {
        if self.step == Step::NewWithConfirm && self.confirming {
            self.confirm_purpose
        } else {
            self.purpose
        }
    }

    /// Записывает результат и просит eframe закрыть окно.
    fn finish(&mut self, value: Option<String>, ui: &egui::Ui) {
        self.done = true;
        if let Ok(mut guard) = self.slot.lock() {
            *guard = Some(value);
        }
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Обрабатывает клавиатуру и вставку: только цифры, Backspace/Delete, Enter, Esc.
    fn handle_input(&mut self, ui: &egui::Ui) {
        if self.done {
            return;
        }

        let events = ui.input(|i| i.events.clone());
        let mut submit = false;

        for event in events {
            match event {
                // Цифры приходят как текст: учитывается раскладка и цифровой блок.
                egui::Event::Text(text) | egui::Event::Paste(text) => {
                    self.buffer.push_text(&text);
                    self.mismatch = false;
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat,
                    ..
                } => match key {
                    egui::Key::Backspace => {
                        self.buffer.pop();
                        self.mismatch = false;
                    }
                    egui::Key::Delete => {
                        self.buffer.clear();
                        self.mismatch = false;
                    }
                    egui::Key::Enter => {
                        if !repeat {
                            submit = true;
                        }
                    }
                    egui::Key::Escape => {
                        self.finish(None, ui);
                        return;
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if submit {
            self.submit(ui);
        }
    }

    /// Подтверждает ввод: для `Single` — сразу, для `NewWithConfirm` — по шагам.
    fn submit(&mut self, ui: &egui::Ui) {
        if !self.buffer.is_full() {
            return;
        }
        let value = self.buffer.as_str().to_string();

        match self.step {
            Step::Single => self.finish(Some(value), ui),
            Step::NewWithConfirm => {
                if self.confirming {
                    if self.first.as_deref() == Some(value.as_str()) {
                        self.finish(Some(value), ui);
                    } else {
                        self.first = None;
                        self.confirming = false;
                        self.buffer.clear();
                        self.mismatch = true;
                    }
                } else {
                    self.first = Some(value);
                    self.confirming = true;
                    self.buffer.clear();
                }
            }
        }
    }

    /// Рисует окно: заголовок, четыре квадратных поля по центру, подсказки.
    fn paint(&self, ui: &egui::Ui) {
        let visuals = ui.visuals().clone();
        let painter = ui.painter();
        let rect = ui.ctx().viewport_rect();
        let center_x = rect.center().x;

        painter.rect_filled(rect, 0.0, visuals.panel_fill);

        let (title, prompt) = self.texts.prompt(self.current_purpose());
        painter.text(
            egui::pos2(center_x, 30.0),
            egui::Align2::CENTER_CENTER,
            title,
            egui::FontId::proportional(19.0),
            visuals.strong_text_color(),
        );
        painter.text(
            egui::pos2(center_x, 60.0),
            egui::Align2::CENTER_CENTER,
            prompt,
            egui::FontId::proportional(13.0),
            visuals.weak_text_color(),
        );

        let total = PIN_LEN as f32 * FIELD + (PIN_LEN as f32 - 1.0) * GAP;
        let left = center_x - total / 2.0;
        let active = if self.buffer.is_full() {
            PIN_LEN
        } else {
            self.buffer.len()
        };

        for idx in 0..PIN_LEN {
            let field = egui::Rect::from_min_size(
                egui::pos2(left + idx as f32 * (FIELD + GAP), FIELDS_TOP),
                egui::vec2(FIELD, FIELD),
            );
            painter.rect_filled(field, CORNER, visuals.extreme_bg_color);

            let stroke = if idx == active {
                visuals.selection.stroke
            } else {
                visuals.widgets.inactive.bg_stroke
            };
            painter.rect_stroke(field, CORNER, stroke, egui::StrokeKind::Inside);

            match self.buffer.digit(idx) {
                Some(digit) => {
                    painter.text(
                        field.center(),
                        egui::Align2::CENTER_CENTER,
                        digit,
                        egui::FontId::monospace(30.0),
                        visuals.strong_text_color(),
                    );
                }
                None => {
                    painter.circle_filled(field.center(), 3.0, visuals.weak_text_color());
                }
            }
        }

        if self.mismatch {
            painter.text(
                egui::pos2(center_x, 176.0),
                egui::Align2::CENTER_CENTER,
                self.texts.mismatch,
                egui::FontId::proportional(12.5),
                visuals.error_fg_color,
            );
        }

        painter.text(
            egui::pos2(center_x, 214.0),
            egui::Align2::CENTER_CENTER,
            self.texts.hint,
            egui::FontId::proportional(11.0),
            visuals.weak_text_color(),
        );
    }
}

impl eframe::App for PinApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_input(ui);
        self.paint(ui);
    }
}

/// Открывает окно и возвращает введённый 4-значный PIN.
///
/// * `Ok(Some(pin))` — PIN введён (ровно [`PIN_LEN`] цифр);
/// * `Ok(None)` — пользователь отменил ввод (Esc или закрытие окна);
/// * `Err(_)` — окно открыть не удалось (headless/CI/SSH) — нужен терминальный ввод.
///
/// Вызывать только из главного потока процесса: окно eframe создаёт winit-цикл
/// событий, которому требуется главный поток ОС.
pub fn ask_pin(purpose: PinPurpose) -> anyhow::Result<Option<String>> {
    run_window(purpose, purpose, Step::Single)
}

/// Открывает окно и запрашивает новый PIN дважды, сверяя значения на месте.
///
/// Семантика результата — как у [`ask_pin`].
pub fn ask_pin_twice(
    purpose: PinPurpose,
    confirm_purpose: PinPurpose,
) -> anyhow::Result<Option<String>> {
    run_window(purpose, confirm_purpose, Step::NewWithConfirm)
}

/// Общий запуск окна для обоих сценариев.
fn run_window(
    purpose: PinPurpose,
    confirm_purpose: PinPurpose,
    step: Step,
) -> anyhow::Result<Option<String>> {
    if gui_disabled_by_env() {
        anyhow::bail!("графический ввод PIN отключён переменной OB2H_PIN_GUI");
    }

    let lang = Lang::resolve();
    let slot: Slot = Arc::new(Mutex::new(None));
    let slot_for_app = Arc::clone(&slot);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(Texts::of(lang).window_title)
            .with_inner_size([WIN_W, WIN_H])
            .with_min_inner_size([WIN_W, WIN_H])
            .with_max_inner_size([WIN_W, WIN_H])
            .with_resizable(false)
            .with_always_on_top(),
        centered: true,
        ..Default::default()
    };

    // Паника winit (не главный поток, отсутствие дисплея) должна превращаться в
    // ошибку, а не ронять CLI — иначе не сработает терминальный fallback.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            "ob2h-pin",
            options,
            Box::new(move |_cc: &eframe::CreationContext<'_>| {
                Ok(Box::new(PinApp::new(
                    lang,
                    step,
                    purpose,
                    confirm_purpose,
                    slot_for_app,
                )) as Box<dyn eframe::App>)
            }),
        )
    }));

    match outcome {
        Ok(Ok(())) => {
            // `None` (окно закрыто без решения) и `Some(None)` (Esc) — одна и та же отмена.
            let value = slot
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().cloned().flatten());
            Ok(value)
        }
        Ok(Err(err)) => anyhow::bail!("не удалось открыть окно ввода PIN: {err}"),
        Err(_) => anyhow::bail!("окно ввода PIN недоступно (нет графической подсистемы)"),
    }
}
