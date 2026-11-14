//! Single-instance lockfile окна просмотра (Ф70.1): `data/viewer.lock` с pid.
//!
//! При старте проверяем живость pid из lockfile: живой процесс — отказ,
//! мёртвый — перехват брошенного lock. Запись lockfile — единственная файловая
//! операция viewer вне БД (сама БД при просмотре не меняется).

use std::fs;
use std::path::{Path, PathBuf};

/// Жив ли процесс с таким pid (OS-специфично, без тяжёлых зависимостей).
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, FALSE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // STILL_ACTIVE (259) — код «процесс ещё работает» из WinBase.h.
        const STILL_ACTIVE: u32 = 259;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
            if handle.is_null() {
                // ERROR_ACCESS_DENIED (5): процесс существует, но защищён — считаем живым.
                return GetLastError() == 5;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }
    #[cfg(unix)]
    {
        if pid > i32::MAX as u32 {
            return false;
        }
        // kill(pid, 0) — только проверка существования, сигнал не доставляется.
        let r = unsafe { libc::kill(pid as i32, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(any(windows, unix)))]
    {
        // Неизвестная ОС — консервативно считаем живым (не перехватываем lock).
        true
    }
}

/// Захваченный lock: удаляет файл при Drop (нормальное закрытие окна).
#[derive(Debug)]
pub struct ViewerLock {
    path: PathBuf,
}

impl ViewerLock {
    /// Захватить lock. `Err` — уже запущен живой viewer (Ф70.1 single-instance).
    pub fn acquire(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join("viewer.lock");
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(pid) = text.trim().parse::<u32>() {
                if pid_alive(pid) {
                    anyhow::bail!(
                        "[Error] окно просмотра уже открыто (pid {pid}) \
                         [Подсказка] закройте существующее окно; если процесс мёртв — удалите {}",
                        path.display()
                    );
                }
            }
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, std::process::id().to_string())?;
        // Гонка двух одновременных стартов: побеждает последний записавший pid.
        let written = fs::read_to_string(&path).unwrap_or_default();
        if written.trim().parse::<u32>().ok() != Some(std::process::id()) {
            anyhow::bail!(
                "[Error] окно просмотра уже открыто (параллельный старт) \
                 [Подсказка] закройте существующее окно"
            );
        }
        Ok(Self { path })
    }

    /// Держит ли живой процесс lockfile (без захвата) — для `viewer_open`.
    pub fn is_running(data_dir: &Path) -> bool {
        let path = data_dir.join("viewer.lock");
        fs::read_to_string(&path)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .map(pid_alive)
            .unwrap_or(false)
    }
}

impl Drop for ViewerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_pid_is_alive_zero_is_not() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
        // Заведомо несуществующий pid (> i32::MAX безопасен и на Unix).
        assert!(!pid_alive(4_000_000_000));
    }

    #[test]
    fn acquire_blocks_second_and_releases_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = ViewerLock::acquire(tmp.path()).unwrap();
        assert!(tmp.path().join("viewer.lock").exists());
        assert!(ViewerLock::is_running(tmp.path()));
        let err = ViewerLock::acquire(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("[Error]"), "{err}");
        drop(lock);
        assert!(!tmp.path().join("viewer.lock").exists());
        assert!(!ViewerLock::is_running(tmp.path()));
    }

    #[test]
    fn stale_lock_of_dead_process_is_taken_over() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("viewer.lock"), "4000000000").unwrap();
        let _lock = ViewerLock::acquire(tmp.path()).unwrap();
        let text = fs::read_to_string(tmp.path().join("viewer.lock")).unwrap();
        assert_eq!(text.trim().parse::<u32>().unwrap(), std::process::id());
    }
}