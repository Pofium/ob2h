//! OB2H (OmnesBot to Hermes) на Rust.
//! Локальное персональное MCP-хранилище знаний для агента Hermes.

pub mod backup;
pub mod cli;
pub mod config;
pub mod consolidator;
pub mod db;
pub mod dream;
pub mod embedding;
pub mod extractor;
pub mod graph;
pub mod ingest;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod sync;
pub mod vector;
pub mod workspace;

use std::sync::Arc;
use tokio::sync::Mutex;

use backup::BackupManager;
use config::Settings;
use consolidator::{Consolidator, PendingSession};
use db::Database;
use dream::{AutoDreamWorker, Dream};
use embedding::provider_for;
use graph::GraphService;
use llm::make_llm;
use mcp::AppContext;
use sync::SyncManager;
use memory::MemoryService;
use workspace::{GitStore, Workspace};

/// Инициализация контекста приложения
pub fn init_app(settings: Settings) -> anyhow::Result<Arc<AppContext>> {
    settings.ensure_dirs()?;

    let db = Database::new(settings.db_path())?;
    let embedder = provider_for(&settings);
    let memory = Arc::new(MemoryService::new(db.clone(), embedder.clone()));
    let workspace = Arc::new(Workspace::new(settings.workspace_dir()));
    let gitstore = Arc::new(GitStore::new(settings.workspace_dir()));
    let llm = make_llm(&settings);
    let consolidator = Arc::new(Consolidator::new(workspace.clone(), llm.clone(), settings.clone()));
    let pending_session = Arc::new(Mutex::new(PendingSession::new()));
    let graph = Arc::new(GraphService::new(db.clone(), embedder.clone()));
    let dream = Arc::new(Dream::new(
        workspace.clone(),
        gitstore.clone(),
        llm.clone(),
        settings.clone(),
        db.clone(),
        Some(graph.clone()),
    ));
    let backup = Arc::new(BackupManager::new(settings.clone(), db.clone(), 14));
    let sync = Arc::new(SyncManager::new(
        settings.clone(),
        db.clone(),
        embedder.clone(),
        backup.clone(),
    ));
    let dream_lock = Arc::new(Mutex::new(()));

    Ok(Arc::new(AppContext {
        settings,
        db,
        embedder,
        memory,
        workspace,
        gitstore,
        llm,
        consolidator,
        pending_session,
        graph,
        dream,
        backup,
        sync,
        dream_lock,
    }))
}

/// Запуск фоновых воркеров (AutoDream)
pub fn start_background_workers(ctx: Arc<AppContext>) {
    if ctx.settings.autodream_enabled {
        let worker = Arc::new(AutoDreamWorker::new(
            ctx.dream.clone(),
            ctx.workspace.clone(),
            ctx.memory.clone(),
            ctx.settings.clone(),
            Some(ctx.sync.clone()),
        ));
        worker.start();
    }
}
