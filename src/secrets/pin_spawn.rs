//! Issue #1: PIN-диалог сейфа из MCP через spawn-хелпер.
//!
//! MCP-сервер — stdio-процесс: окно eframe требует главный поток ОС, которого у
//! сервера нет. Решение — тот же бинарник подкомандой `ob2h secrets ask-pin`:
//! новый процесс = чистый главный поток, PIN уходит в stdout родителя (локальный
//! pipe), сервер выводит из него ключ (Argon2id → verifier) и разблокирует себя.
//! PIN не пишется в логи/аудит/чат (ADR-18/ADR-21).
//!
//! Гейты: gui-сборка и `OB2H_PIN_GUI!=0` — иначе `Unavailable` и прежнее
//! поведение v2.0.0 (ошибка locked с подсказкой про CLI). Таймаут окна —
//! `OB2H_PIN_TIMEOUT_SECS` (дефолт 60).

use std::process::Stdio;

/// Итог PIN-диалога из MCP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinPromptOutcome {
    /// Пользователь ввёл PIN (ровно 4 цифры — формат enforced окном).
    Pin(String),
    /// Окно закрыто/Esc — без ретраев окна.
    Cancelled,
    /// Окно недоступно (headless, без-gui сборка, `OB2H_PIN_GUI=0`, таймаут,
    /// хелпер не запустился) — агенту возвращается прежняя locked-ошибка.
    Unavailable,
}

/// Дефолтный таймаут окна PIN-диалога, сек.
const DEFAULT_PIN_TIMEOUT_SECS: u64 = 60;
/// Минимальный таймаут (защита от `OB2H_PIN_TIMEOUT_SECS=0/1` — агент не висит,
/// но и окно не мгновенно гаснет).
const MIN_PIN_TIMEOUT_SECS: u64 = 5;

/// Таймаут окна из env `OB2H_PIN_TIMEOUT_SECS` (битое/нулевое — дефолт/минимум).
pub fn timeout_secs_from_env() -> u64 {
    match std::env::var("OB2H_PIN_TIMEOUT_SECS") {
        Ok(v) => v
            .trim()
            .parse::<u64>()
            .map(|s| s.max(MIN_PIN_TIMEOUT_SECS))
            .unwrap_or(DEFAULT_PIN_TIMEOUT_SECS),
        Err(_) => DEFAULT_PIN_TIMEOUT_SECS,
    }
}

/// Гейт спавна окна из MCP (issue #1): только gui-сборка и `OB2H_PIN_GUI!=0`.
/// В без-gui сборке окно недостижимо в принципе (тот же бинарник) — не спавним.
pub fn spawn_allowed() -> bool {
    #[cfg(feature = "gui")]
    {
        !crate::secrets::pin_input::gui_disabled_by_env()
    }
    #[cfg(not(feature = "gui"))]
    {
        false
    }
}

/// Чистая логика решения по исходу диалога (вынесена для тестов):
/// `Ok(pin)` — разблокировать сейф и повторить операцию; `Err(msg)` — вернуть
/// агенту ошибку (cancelled — своя, unavailable — прежняя locked-ошибка).
pub fn dialog_decision(outcome: &PinPromptOutcome, locked_err: &str) -> Result<String, String> {
    match outcome {
        PinPromptOutcome::Pin(pin) => Ok(pin.clone()),
        PinPromptOutcome::Cancelled => Err("[Error] ввод PIN отменён пользователем".to_string()),
        PinPromptOutcome::Unavailable => Err(locked_err.to_string()),
    }
}

/// Парсинг JSON-вывода хелпера (последняя непустая строка stdout).
/// Битый/чужой вывод — `Unavailable` (без паник на мусоре).
pub fn parse_outcome(stdout: &str) -> PinPromptOutcome {
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return PinPromptOutcome::Unavailable;
    };
    match v.get("outcome").and_then(|x| x.as_str()) {
        Some("pin") => {
            let pin = v.get("pin").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if pin.is_empty() {
                PinPromptOutcome::Unavailable
            } else {
                PinPromptOutcome::Pin(pin)
            }
        }
        Some("cancelled") => PinPromptOutcome::Cancelled,
        _ => PinPromptOutcome::Unavailable,
    }
}

/// Запускает хелпер PIN-диалога (`current_exe secrets ask-pin --purpose unlock`)
/// и ждёт результат до таймаута.
pub async fn prompt_pin_via_helper() -> PinPromptOutcome {
    if !spawn_allowed() {
        return PinPromptOutcome::Unavailable;
    }
    let Ok(exe) = std::env::current_exe() else {
        return PinPromptOutcome::Unavailable;
    };
    let timeout = timeout_secs_from_env();
    let mut child = tokio::process::Command::new(exe);
    child
        .args(["secrets", "ask-pin", "--purpose", "unlock"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let fut = async {
        match child.output().await {
            Ok(out) => parse_outcome(&String::from_utf8_lossy(&out.stdout)),
            Err(_) => PinPromptOutcome::Unavailable,
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(timeout), fut).await {
        Ok(outcome) => outcome,
        // Таймаут: окно убито (kill_on_drop) — агенту прежняя locked-ошибка.
        Err(_) => PinPromptOutcome::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #1: таймаут окна — env-конфиг с дефолтом и защитой от 0/мусора.
    #[test]
    fn timeout_from_env_defaults_and_clamps() {
        // Установка env в тестах не параллелится с другими тестами этого модуля
        // (cargo test по умолчанию — потоки, но тесты ниже читают тот же ключ —
        // оставляем последовательность через один set/remove в каждом тесте).
        std::env::remove_var("OB2H_PIN_TIMEOUT_SECS");
        assert_eq!(timeout_secs_from_env(), 60);
        std::env::set_var("OB2H_PIN_TIMEOUT_SECS", "90");
        assert_eq!(timeout_secs_from_env(), 90);
        std::env::set_var("OB2H_PIN_TIMEOUT_SECS", "abc");
        assert_eq!(timeout_secs_from_env(), 60);
        std::env::set_var("OB2H_PIN_TIMEOUT_SECS", "1");
        assert_eq!(timeout_secs_from_env(), 5);
        std::env::set_var("OB2H_PIN_TIMEOUT_SECS", "0");
        assert_eq!(timeout_secs_from_env(), 5);
        std::env::remove_var("OB2H_PIN_TIMEOUT_SECS");
    }

    /// Issue #1: парсинг JSON-вывода хелпера — все исходы + мусор.
    #[test]
    fn parse_outcome_all_branches_and_garbage() {
        assert_eq!(
            parse_outcome("{\"outcome\":\"pin\",\"pin\":\"1234\"}\n"),
            PinPromptOutcome::Pin("1234".to_string())
        );
        assert_eq!(
            parse_outcome("служебный шум\n{\"outcome\":\"cancelled\"}\n"),
            PinPromptOutcome::Cancelled
        );
        assert_eq!(
            parse_outcome("{\"outcome\":\"unavailable\"}\n"),
            PinPromptOutcome::Unavailable
        );
        // pin без значения, битый JSON, пустой вывод — недоступно.
        assert_eq!(parse_outcome("{\"outcome\":\"pin\"}"), PinPromptOutcome::Unavailable);
        assert_eq!(parse_outcome("не json"), PinPromptOutcome::Unavailable);
        assert_eq!(parse_outcome(""), PinPromptOutcome::Unavailable);
    }

    /// Issue #1: решение по исходу диалога — pin к разблокировке, cancelled —
    /// своя ошибка без ретраев, unavailable — прежняя locked-ошибка.
    #[test]
    fn dialog_decision_maps_outcomes() {
        let locked = "[Error] secrets locked: запустите \"ob2h secrets unlock\"";
        assert_eq!(
            dialog_decision(&PinPromptOutcome::Pin("9999".into()), locked),
            Ok("9999".to_string())
        );
        assert_eq!(
            dialog_decision(&PinPromptOutcome::Cancelled, locked),
            Err("[Error] ввод PIN отменён пользователем".to_string())
        );
        assert_eq!(
            dialog_decision(&PinPromptOutcome::Unavailable, locked),
            Err(locked.to_string())
        );
    }

    /// Issue #1: гейт спавна — OB2H_PIN_GUI=0 выключает диалог всегда
    /// (и в gui-сборке); без env — решает фича-гейт сборки.
    #[test]
    fn spawn_gate_respects_pin_gui_env() {
        std::env::set_var("OB2H_PIN_GUI", "0");
        assert!(!spawn_allowed());
        std::env::remove_var("OB2H_PIN_GUI");
        // В без-gui сборке спавн никогда не разрешён (окно недостижимо).
        #[cfg(not(feature = "gui"))]
        assert!(!spawn_allowed());
    }
}
