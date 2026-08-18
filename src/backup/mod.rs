//! Бэкапы: атомарный снимок БД (VACUUM INTO) + воркспейса с ротацией N=14.

use std::fs;
use std::path::{Path, PathBuf};
use chrono::Utc;
use rusqlite::params;
use tracing::info;

use crate::config::Settings;
use crate::db::Database;

pub const DEFAULT_KEEP: usize = 14;

pub struct BackupManager {
    settings: Settings,
    db: Database,
    keep: usize,
}

impl BackupManager {
    pub fn new(settings: Settings, db: Database, keep: usize) -> Self {
        Self {
            settings,
            db,
            keep,
        }
    }

    pub fn create(&self) -> anyhow::Result<PathBuf> {
        let stamp = Utc::now().format("%Y-%m-%d_%H%M%S").to_string();
        let target = self.settings.backups_dir().join(&stamp);
        fs::create_dir_all(&target)?;

        let dest_db = target.join("ob2h.db");
        let dest_db_str = dest_db.to_string_lossy().to_string();

        // VACUUM INTO создаёт атомарную копию живой БД
        self.db.with_conn(|conn| {
            conn.execute("VACUUM INTO ?1", params![dest_db_str])?;
            Ok(())
        })?;

        // Копируем воркспейс
        let workspace_src = self.settings.workspace_dir();
        let workspace_dest = target.join("workspace");
        if workspace_src.exists() {
            copy_dir_all(&workspace_src, &workspace_dest)?;
        }

        self.rotate()?;
        info!("Бэкап создан: {}", target.display());
        Ok(target)
    }

    pub fn rotate(&self) -> anyhow::Result<usize> {
        let backups_dir = self.settings.backups_dir();
        if !backups_dir.exists() {
            return Ok(0);
        }

        let mut dirs: Vec<PathBuf> = fs::read_dir(&backups_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();

        dirs.sort_by_key(|p| p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string());

        let mut removed = 0;
        if dirs.len() > self.keep {
            let to_remove = dirs.len() - self.keep;
            for p in &dirs[..to_remove] {
                let _ = fs::remove_dir_all(p);
                removed += 1;
            }
        }

        Ok(removed)
    }

    pub fn list(&self) -> Vec<String> {
        let backups_dir = self.settings.backups_dir();
        if !backups_dir.exists() {
            return Vec::new();
        }

        let mut names: Vec<String> = fs::read_dir(backups_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();

        names.sort();
        names
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let file_name = entry.file_name();
        if file_name == "autodream.lock" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
