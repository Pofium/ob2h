//! Фоновый воркер периодической синхронизации между PC и VPS.

use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::sync::SyncManager;

pub struct AutoSyncWorker {
    sync: Arc<SyncManager>,
    interval_minutes: u64,
    enabled: bool,
}

impl AutoSyncWorker {
    pub fn new(sync: Arc<SyncManager>, interval_minutes: u64, enabled: bool) -> Self {
        Self {
            sync,
            interval_minutes,
            enabled,
        }
    }

    pub fn start(self: Arc<Self>) {
        if !self.enabled || self.interval_minutes == 0 {
            info!("AutoSync: отключён настройками");
            return;
        }

        tokio::spawn(async move {
            info!("AutoSync: запущен (интервал {} мин)", self.interval_minutes);
            let mut interval = tokio::time::interval(Duration::from_secs(self.interval_minutes * 60));
            // Первый тик срабатывает немедленно — пропускаем, чтобы дать серверу инициализироваться
            interval.tick().await;

            loop {
                interval.tick().await;

                let peers: Vec<String> = self
                    .sync
                    .node_config()
                    .peers
                    .iter()
                    .filter(|(_, p)| p.method == "ssh")
                    .map(|(n, _)| n.clone())
                    .collect();

                if peers.is_empty() {
                    continue;
                }

                info!("AutoSync: запуск плановой синхронизации с пирингами: {:?}", peers);
                for peer in &peers {
                    if let Err(e) = self.sync.push(peer) {
                        warn!("AutoSync push '{}': {e}", peer);
                    }
                    if let Err(e) = self.sync.pull(peer).await {
                        warn!("AutoSync pull '{}': {e}", peer);
                    }
                }
            }
        });
    }
}
