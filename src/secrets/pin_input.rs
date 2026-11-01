//! Модель ввода и локализация окна PIN-кода (чистая логика, без GUI).
//!
//! Модуль компилируется всегда: язык интерфейса, буфер цифр и строки RU/EN
//! тестируются без графического окружения. Само окно — в [`super::pin_gui`]
//! (feature `gui`).

/// Длина PIN-кода: ровно 4 цифры.
pub const PIN_LEN: usize = 4;

/// Сообщение об ошибке формата PIN (инициализация сейфа).
pub const PIN_FORMAT_ERROR: &str = "PIN-код должен состоять ровно из 4 цифр";

/// Сообщение об ошибке формата нового PIN (смена кода).
pub const NEW_PIN_FORMAT_ERROR: &str = "новый PIN-код должен состоять ровно из 4 цифр";

/// Соответствует ли PIN формату: ровно [`PIN_LEN`] ASCII-цифр.
pub fn is_valid_pin(pin: &str) -> bool {
    pin.len() == PIN_LEN && pin.bytes().all(|b| b.is_ascii_digit())
}

/// Назначение запроса PIN — влияет на заголовок и подсказку окна.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinPurpose {
    /// Новый PIN при инициализации хранилища.
    New,
    /// Повтор нового PIN (подтверждение совпадения).
    Confirm,
    /// Запрос PIN для разблокировки/операции над хранилищем.
    Unlock,
    /// Текущий PIN при смене кода.
    Current,
}

/// Язык интерфейса окна.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    /// Русский.
    Ru,
    /// Английский.
    En,
}

impl Lang {
    /// Язык приложения: env-оверрайд `OB2H_UI_LANG` → системная локаль → английский.
    pub fn resolve() -> Self {
        let env_tag = std::env::var("OB2H_UI_LANG").ok();
        #[cfg(feature = "gui")]
        let sys_tag = sys_locale::get_locale();
        #[cfg(not(feature = "gui"))]
        let sys_tag: Option<String> = None;
        Self::resolve_from(env_tag.as_deref(), sys_tag.as_deref())
    }

    /// Чистая логика выбора языка (вынесена отдельно для тестов).
    ///
    /// Приоритет: первый распознанный тег из `env_tag`, затем из `locale_tag`,
    /// иначе — [`Lang::En`].
    pub fn resolve_from(env_tag: Option<&str>, locale_tag: Option<&str>) -> Self {
        for tag in [env_tag, locale_tag].into_iter().flatten() {
            let tag = tag.trim().to_ascii_lowercase();
            if tag.starts_with("ru") {
                return Self::Ru;
            }
            if tag.starts_with("en") {
                return Self::En;
            }
        }
        Self::En
    }
}

/// Локализованные строки интерфейса.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texts {
    /// Язык, для которого подобраны строки.
    pub lang: Lang,
    /// Заголовок окна.
    pub window_title: &'static str,
    /// Подсказка по управлению окном.
    pub hint: &'static str,
    /// Сообщение о несовпадении PIN при подтверждении.
    pub mismatch: &'static str,
}

impl Texts {
    /// Набор строк для языка.
    pub fn of(lang: Lang) -> Self {
        match lang {
            Lang::Ru => Self {
                lang,
                window_title: "ob2h · Хранилище секретов",
                hint: "Enter — подтвердить     Esc — отмена",
                mismatch: "PIN-коды не совпадают, введите заново",
            },
            Lang::En => Self {
                lang,
                window_title: "ob2h · Secrets Vault",
                hint: "Enter — confirm     Esc — cancel",
                mismatch: "PIN codes do not match, try again",
            },
        }
    }

    /// Заголовок и подсказка для конкретного назначения ввода.
    pub fn prompt(&self, purpose: PinPurpose) -> (&'static str, &'static str) {
        match (self.lang, purpose) {
            (Lang::Ru, PinPurpose::New) => ("Новый PIN-код", "Придумайте 4-значный PIN-код"),
            (Lang::Ru, PinPurpose::Confirm) => ("Подтверждение", "Повторите PIN-код"),
            (Lang::Ru, PinPurpose::Unlock) => ("Разблокировка", "Введите 4-значный PIN-код"),
            (Lang::Ru, PinPurpose::Current) => ("Текущий PIN-код", "Введите действующий PIN-код"),
            (Lang::En, PinPurpose::New) => ("New PIN", "Choose a 4-digit PIN"),
            (Lang::En, PinPurpose::Confirm) => ("Confirm PIN", "Re-enter the PIN"),
            (Lang::En, PinPurpose::Unlock) => ("Unlock", "Enter your 4-digit PIN"),
            (Lang::En, PinPurpose::Current) => ("Current PIN", "Enter your current PIN"),
        }
    }
}

/// Буфер ввода PIN: принимает только ASCII-цифры, не длиннее [`PIN_LEN`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PinBuffer {
    digits: String,
}

impl PinBuffer {
    /// Пустой буфер.
    pub fn new() -> Self {
        Self::default()
    }

    /// Количество введённых цифр.
    pub fn len(&self) -> usize {
        self.digits.len()
    }

    /// Буфер пуст.
    pub fn is_empty(&self) -> bool {
        self.digits.is_empty()
    }

    /// Введены все [`PIN_LEN`] цифры.
    pub fn is_full(&self) -> bool {
        self.digits.len() >= PIN_LEN
    }

    /// Введённые цифры.
    pub fn as_str(&self) -> &str {
        &self.digits
    }

    /// Очищает буфер.
    pub fn clear(&mut self) {
        self.digits.clear();
    }

    /// Удаляет последнюю цифру.
    pub fn pop(&mut self) {
        self.digits.pop();
    }

    /// Добавляет цифры из текста или вставки, игнорируя все прочие символы.
    pub fn push_text(&mut self, text: &str) {
        for ch in text.chars() {
            if self.is_full() {
                break;
            }
            if ch.is_ascii_digit() {
                self.digits.push(ch);
            }
        }
    }

    /// Цифра по индексу (0-based), если она уже введена.
    pub fn digit(&self, idx: usize) -> Option<char> {
        // В буфере только ASCII-цифры, поэтому байт совпадает с символом.
        self.digits.as_bytes().get(idx).map(|b| *b as char)
    }
}

/// GUI отключён пользователем через `OB2H_PIN_GUI=0|false|off|no|none`.
pub fn gui_disabled_by_env() -> bool {
    match std::env::var("OB2H_PIN_GUI") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no" | "none"
        ),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_env_override_wins() {
        assert_eq!(Lang::resolve_from(Some("ru-RU"), Some("en-US")), Lang::Ru);
        assert_eq!(Lang::resolve_from(Some("en-GB"), Some("ru-RU")), Lang::En);
    }

    #[test]
    fn lang_falls_back_to_locale_then_english() {
        assert_eq!(Lang::resolve_from(None, Some("ru-RU")), Lang::Ru);
        assert_eq!(Lang::resolve_from(Some("  "), Some("RU")), Lang::Ru);
        assert_eq!(Lang::resolve_from(None, Some("de-DE")), Lang::En);
        assert_eq!(Lang::resolve_from(None, None), Lang::En);
    }

    #[test]
    fn texts_are_localized_for_both_languages() {
        let ru = Texts::of(Lang::Ru);
        let en = Texts::of(Lang::En);
        assert_ne!(ru.window_title, en.window_title);
        for purpose in [
            PinPurpose::New,
            PinPurpose::Confirm,
            PinPurpose::Unlock,
            PinPurpose::Current,
        ] {
            assert_ne!(ru.prompt(purpose), en.prompt(purpose));
        }
    }

    #[test]
    fn buffer_accepts_digits_only_and_stops_at_4() {
        let mut buf = PinBuffer::new();
        buf.push_text("1a2b3c4d5");
        assert_eq!(buf.as_str(), "1234");
        assert!(buf.is_full());
        assert_eq!(buf.digit(2), Some('3'));
        assert_eq!(buf.digit(4), None);
    }

    #[test]
    fn pin_format_is_exactly_four_digits() {
        for ok in ["0000", "1234", "9999"] {
            assert!(is_valid_pin(ok), "{ok} должен приниматься");
        }
        for bad in ["", "123", "12345", "12a4", "１２３４", " 1234", "1234 "] {
            assert!(!is_valid_pin(bad), "{bad} должен отклоняться");
        }
    }

    #[test]
    fn buffer_pop_and_clear() {
        let mut buf = PinBuffer::new();
        assert!(buf.is_empty());
        buf.push_text("12");
        buf.pop();
        assert_eq!(buf.as_str(), "1");
        buf.clear();
        assert!(buf.is_empty());
        buf.pop();
        assert!(buf.is_empty());
    }
}
