//! Фоновый наблюдатель за изменениями файлов проекта (File Watcher).
//! Использует notify и debouncer для реактивного инкрементального обновления графа кодовой базы.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use tracing::{error, info, warn};

use crate::project::ProjectService;

/// Проверяет, является ли путь исходным кодом поддерживаемого языка
pub fn is_code_file(path: &Path) -> bool {
    for comp in path.components() {
        let name = comp.as_os_str().to_string_lossy();
        if matches!(
            name.as_ref(),
            ".git" | "target" | "node_modules" | "dist" | "build" | "venv" | ".venv" | "data" | "logs" | "backups" | "__pycache__"
        ) {
            return false;
        }
    }

    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        let ext_lower = ext.to_lowercase();
        matches!(
            ext_lower.as_str(),
            "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "sql"
            | "c" | "cpp" | "h" | "hpp" | "php" | "dart" | "java"
        )
    } else {
        false
    }
}

type WatchTarget = (String, PathBuf, tokio::sync::oneshot::Sender<()>);

pub struct ProjectWatcher {
    project_service: Arc<ProjectService>,
    debounce_ms: u64,
    current_watch: Arc<tokio::sync::Mutex<Option<WatchTarget>>>,
}

impl ProjectWatcher {
    pub fn new(project_service: Arc<ProjectService>, debounce_ms: u64) -> Self {
        Self {
            project_service,
            debounce_ms,
            current_watch: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Начать наблюдение за проектом. Если уже наблюдался другой проект, старый останавливается.
    pub async fn switch_project(&self, project_id: &str, root_path: &Path) -> anyhow::Result<()> {
        let mut cur = self.current_watch.lock().await;
        if let Some((cur_id, cur_path, _)) = cur.as_ref() {
            if cur_id == project_id && cur_path == root_path {
                return Ok(()); // уже наблюдается этот же путь
            }
        }

        // Останавливаем предыдущий воркер через oneshot
        if let Some((old_id, _, stop_tx)) = cur.take() {
            let _ = stop_tx.send(());
            info!("FileWatcher: остановлено наблюдение за проектом '{old_id}'");
        }

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let project_id_owned = project_id.to_string();
        let root_path_owned = root_path.to_path_buf();
        let service = self.project_service.clone();
        let debounce_dur = Duration::from_millis(self.debounce_ms);

        let p_id_for_thread = project_id_owned.clone();
        let r_path_for_thread = root_path_owned.clone();

        // Канал для передачи пакетов событий из debouncer-колбэка в асинхронный воркер
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Vec<PathBuf>>(100);

        std::thread::Builder::new()
            .name(format!("ob2h-watcher-{}", project_id_owned))
            .spawn(move || {
                let (sync_tx, sync_rx) = std::sync::mpsc::channel();
                let mut debouncer = match new_debouncer(debounce_dur, sync_tx) {
                    Ok(d) => d,
                    Err(e) => {
                        error!("FileWatcher: ошибка создания дебаунсера: {e}");
                        return;
                    }
                };

                if let Err(e) = debouncer.watcher().watch(&r_path_for_thread, RecursiveMode::Recursive) {
                    warn!("FileWatcher: не удалось подписаться на каталог {}: {e}", r_path_for_thread.display());
                    return;
                }

                info!("FileWatcher: запущено наблюдение за '{}' ({})", p_id_for_thread, r_path_for_thread.display());

                loop {
                    match sync_rx.recv() {
                        Ok(Ok(events)) => {
                            let relevant_paths: Vec<PathBuf> = events
                                .into_iter()
                                .map(|e| e.path)
                                .filter(|p| is_code_file(p))
                                .collect();

                            if !relevant_paths.is_empty() && event_tx.blocking_send(relevant_paths).is_err() {
                                break; // получатель закрыт
                            }
                        }
                        Ok(Err(errs)) => {
                            warn!("FileWatcher: ошибки файловой системы: {:?}", errs);
                        }
                        Err(_) => break, // канал закрылся
                    }
                }
            })?;

        // Асинхронный воркер для обработки накопившихся событий без блокировки
        let p_id_async = project_id_owned.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => {
                        break;
                    }
                    batch = event_rx.recv() => {
                        match batch {
                            Some(paths) => {
                                info!("FileWatcher: обнаружены изменения в {} файлах проекта '{}', запуск инкрементального сканирования", paths.len(), p_id_async);
                                let svc = service.clone();
                                let pid = p_id_async.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    match svc.scan_project(&pid, None, true) {
                                        Ok(res) => {
                                            info!("FileWatcher: инкрементальное сканирование завершено: scanned={}, nodes={}, edges={}",
                                                res.files_scanned, res.nodes.len(), res.edges.len());
                                        }
                                        Err(e) => {
                                            warn!("FileWatcher: ошибка инкрементального сканирования: {e}");
                                        }
                                    }
                                }).await;
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        *cur = Some((project_id_owned, root_path_owned, stop_tx));
        Ok(())
    }
}
