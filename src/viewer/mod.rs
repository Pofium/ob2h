//! Локальное окно просмотра хранилища (v1.9.9, Фаза 70).
//!
//! Отдельный процесс от `serve` (ADR-7: без новых портов/сервисов): БД открывается
//! **read-only** (`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only`), миграции не
//! запускаются. Единственное санкционированное исключение — таб «Секреты»
//! (Ф71/§5.5): операции идут через [`secrets_ctl`] поверх `SecretsService` и
//! пишут только в `secrets`/`kv`/`audit_log`.
//!
//! Ядро (data/lock/layout/secrets_ctl/texts/theme) компилируется и тестируется
//! без GUI; само окно ([`app`]/[`tabs`]) — только при feature `gui`.

pub mod data;
pub mod layout;
pub mod lock;
pub mod secrets_ctl;
pub mod texts;
pub mod theme;

#[cfg(feature = "gui")]
mod app;
#[cfg(feature = "gui")]
mod tabs;

use std::path::Path;

/// Результат попытки открыть окно из MCP `viewer_open` (Ф72.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenStatus {
    /// Окно запущено detached-процессом.
    Opened,
    /// Живой viewer уже держит lockfile.
    AlreadyRunning,
    /// Сборка без feature `gui` — окно недоступно.
    GuiUnavailable,
}

/// Запускает окно просмотра (CLI `ob2h viewer`).
///
/// Блокирует поток до закрытия окна. В headless-сборке возвращает честную
/// ошибку `[Error] … [Подсказка]` (Ф71.4), не пытаясь открыть окно.
pub fn run(data_dir: &Path, secrets_ttl_secs: u64) -> anyhow::Result<()> {
    #[cfg(not(feature = "gui"))]
    {
        let _ = (data_dir, secrets_ttl_secs);
        anyhow::bail!(
            "[Error] сборка без gui: окно просмотра недоступно \
             [Подсказка] пересоберите ob2h с --features gui либо используйте CLI: \
             ob2h stats, ob2h audit, ob2h secrets status"
        );
    }
    #[cfg(feature = "gui")]
    {
        let db_path = data_dir.join("ob2h.db");
        if !db_path.exists() {
            anyhow::bail!(
                "[Error] БД не найдена: {} [Подсказка] укажите каталог данных через \
                 --data-dir или OB2H_DATA_DIR",
                db_path.display()
            );
        }
        // Single-instance: брошенный lock мёртвого процесса перехватывается.
        let _lock = lock::ViewerLock::acquire(data_dir)?;
        app::run_window(data_dir, secrets_ttl_secs)
    }
}

/// Detached-запуск окна для MCP `viewer_open` (Ф72.1): не блокирует сервер и
/// переживает завершение родительского процесса.
///
/// Windows: `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB` — выход из job-объекта
/// harness'а; если job запрещает breakaway, повторяем без него (Ф72.3 fallback).
/// Unix: своя process-group (`process_group(0)`), чтобы kill-tree родителя не
/// задел окно.
pub fn open_detached(data_dir: &Path) -> anyhow::Result<OpenStatus> {
    if !cfg!(feature = "gui") {
        return Ok(OpenStatus::GuiUnavailable);
    }
    if lock::ViewerLock::is_running(data_dir) {
        return Ok(OpenStatus::AlreadyRunning);
    }

    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("viewer")
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Ok(_) => Ok(OpenStatus::Opened),
            Err(_) => {
                // Job может запрещать breakaway — пробуем просто detached.
                cmd.creation_flags(DETACHED_PROCESS);
                cmd.spawn()?;
                Ok(OpenStatus::Opened)
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        cmd.spawn()?;
        Ok(OpenStatus::Opened)
    }
    #[cfg(not(any(windows, unix)))]
    {
        cmd.spawn()?;
        Ok(OpenStatus::Opened)
    }
}