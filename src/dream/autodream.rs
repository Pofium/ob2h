//! AutoDreamWorker: фоновый worker дриминга на Tokio с гейтами.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::Dream;
use crate::config::Settings;
use crate::memory::MemoryService;
use crate::sync::SyncManager;
use crate::workspace::Workspace;

pub const LOCK_STALE_SECS: u64 = 3600;

pub struct AutoDreamWorker {
    dream: Arc<Dream>,
    workspace: Arc<Workspace>,
    memory: Arc<MemoryService>,
    settings: Settings,
    sync: Option<Arc<SyncManager>>,
    #[allow(dead_code)]
    running: Arc<Mutex<bool>>,
}

impl AutoDreamWorker {
    pub fn new(
        dream: Arc<Dream>,
        workspace: Arc<Workspace>,
        memory: Arc<MemoryService>,
        settings: Settings,
        sync: Option<Arc<SyncManager>>,
    ) -> Self {
        Self {
            dream,
            workspace,
            memory,
            settings,
            sync,
            running: Arc::new(Mutex::new(false)),
        }
    }

    pub fn start(self: Arc<Self>) {
        if !self.settings.autodream_enabled {
            return;
        }

        tokio::spawn(async move {
            let interval_secs = (self.settings.autodream_interval_min * 60).max(60);
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));

            loop {
                ticker.tick().await;
                let (should_run, reason) = self.should_run();
                if !should_run {
                    tracing::debug!("Автодрим пропущен: {reason}");
                    continue;
                }

                if !self.acquire_lock() {
                    continue;
                }

                info!("Запуск автодрима...");
                let result = self.dream.run("auto").await;
                match result {
                    Ok(stats) => info!("Автодрим завершён: status={}", stats.status),
                    Err(e) => warn!("Ошибка автодрима: {e}"),
                }

                self.save_last_run();

                // Регламентное обслуживание памяти (decay + purge + чистка tombstones)
                let _ = self.memory.decay_importance(0.01);
                let _ = self.memory.purge_weak(0.05, 2);
                let _ = self.memory.purge_tombstones(self.settings.retention_days * 2);

                self.prune_daily();
                self.release_lock();

                // after_dream: обмен с пирами (best-effort, не роняет дрим)
                if let Some(sync) = &self.sync {
                    if let Err(e) = sync.run_scheduled().await {
                        warn!("after_dream sync: {e}");
                    }
                }
            }
        });
    }

    fn state_file(&self) -> PathBuf {
        self.settings.data_dir.join("autodream_last_run.json")
    }

    fn lock_file(&self) -> PathBuf {
        self.settings.data_dir.join("autodream.lock")
    }

    fn last_run_iso(&self) -> Option<String> {
        let path = self.state_file();
        let content = fs::read_to_string(path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;
        json.get("last_run").and_then(|v| v.as_str()).map(|s| s.to_string())
    }

    fn save_last_run(&self) {
        let _ = fs::create_dir_all(&self.settings.data_dir);
        let now_iso = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let val = serde_json::json!({ "last_run": now_iso });
        let _ = fs::write(self.state_file(), val.to_string());
    }

    pub fn should_run(&self) -> (bool, String) {
        let now = Utc::now();
        if let Some(last_str) = self.last_run_iso() {
            if let Ok(last_dt) = DateTime::parse_from_rfc3339(&last_str) {
                let elapsed_hours = (now - last_dt.with_timezone(&Utc)).num_minutes() as f64 / 60.0;
                if elapsed_hours < self.settings.autodream_min_interval_h as f64 {
                    return (
                        false,
                        format!("прошло {elapsed_hours:.1}ч < {}ч", self.settings.autodream_min_interval_h),
                    );
                }
            }
        }

        let fresh_events = self.count_daily_events_since(self.last_run_iso().as_deref().unwrap_or(""));
        if fresh_events < self.settings.autodream_min_events {
            return (
                false,
                format!("новых событий {fresh_events} < {}", self.settings.autodream_min_events),
            );
        }

        (true, "ok".to_string())
    }

    fn count_daily_events_since(&self, since_iso: &str) -> usize {
        let daily_dir = self.workspace.daily_dir();
        if !daily_dir.exists() {
            return 0;
        }

        let mut count = 0;
        if let Ok(entries) = fs::read_dir(daily_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if let Ok(content) = fs::read_to_string(entry.path()) {
                        for line in content.lines() {
                            if line.trim().is_empty() {
                                continue;
                            }
                            if since_iso.is_empty() {
                                count += 1;
                            } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                                if let Some(ts) = val.get("timestamp").and_then(|t| t.as_str()) {
                                    if ts > since_iso {
                                        count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        count
    }

    fn acquire_lock(&self) -> bool {
        let lock = self.lock_file();
        if lock.exists() {
            if let Ok(meta) = fs::metadata(&lock) {
                if let Ok(modified) = meta.modified() {
                    if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                        if elapsed.as_secs() < LOCK_STALE_SECS {
                            return false;
                        }
                        warn!("Lock stale ({}s) — перехватываю", elapsed.as_secs());
                    }
                }
            }
        }
        let _ = fs::create_dir_all(&self.settings.data_dir);
        let _ = fs::write(lock, Utc::now().to_rfc3339());
        true
    }

    fn release_lock(&self) {
        let _ = fs::remove_file(self.lock_file());
    }

    pub fn prune_daily(&self) -> usize {
        let cutoff = Utc::now() - ChronoDuration::days(self.settings.retention_days);
        let mut removed = 0;
        let daily_dir = self.workspace.daily_dir();

        if let Ok(entries) = fs::read_dir(daily_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(naive) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
                            let file_date = naive.and_hms_opt(0, 0, 0).unwrap().and_utc();
                            if file_date < cutoff {
                                let _ = fs::remove_file(path);
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }
        removed
    }
}
