//! Бэкапы: полный атомарный снимок БД (VACUUM INTO) + воркспейс, быстрый снимок
//! памяти (quick) и проверка восстановимости (verify). Ф24 PLAN_v1.3.

use std::fs;
use std::path::{Path, PathBuf};
use chrono::Utc;
use rusqlite::params;
use tracing::info;

use crate::config::Settings;
use crate::db::Database;

pub struct BackupManager {
    settings: Settings,
    db: Database,
}

impl BackupManager {
    pub fn new(settings: Settings, db: Database) -> Self {
        Self { settings, db }
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
        self.copy_workspace(&target)?;

        self.rotate()?;
        info!("Бэкап создан: {}", target.display());
        Ok(target)
    }

    /// Ф24: быстрый бэкап памяти — только memories/kv/memory_links + воркспейс
    /// (~10–30 МБ против ~1 ГБ full). Граф не включается: полный снимок по-прежнему
    /// делается `ob2h backup` (full) по расписанию владельца.
    pub fn create_quick(&self) -> anyhow::Result<PathBuf> {
        let stamp = Utc::now().format("%Y-%m-%d_%H%M%S").to_string();
        let target = self.settings.backups_dir().join(format!("quick-{stamp}"));
        fs::create_dir_all(&target)?;

        let dest_db = target.join("ob2h-quick.db");
        let lit = dest_db.to_string_lossy().replace('\'', "''");
        self.db.with_conn(|conn| {
            conn.execute_batch(&format!(
                "ATTACH '{lit}' AS quick_out;
                 CREATE TABLE quick_out.memories AS SELECT * FROM main.memories;
                 CREATE TABLE quick_out.kv AS SELECT * FROM main.kv;
                 CREATE TABLE quick_out.memory_links AS SELECT * FROM main.memory_links;
                 DETACH quick_out;"
            ))?;
            Ok(())
        })?;

        self.copy_workspace(&target)?;
        self.rotate()?;
        info!("Быстрый бэкап создан: {}", target.display());
        Ok(target)
    }

    /// Ф24: verify — «бэкап, который ни разу не восстанавливали, — не бэкап».
    /// Открывает копию read-only, гоняет integrity_check, сверяет счётчики строк
    /// таблиц, присутствующих в копии, с живой БД.
    pub fn verify(&self, path: &Path) -> anyhow::Result<String> {
        let db_file = if path.is_dir() {
            let full = path.join("ob2h.db");
            let quick = path.join("ob2h-quick.db");
            if full.is_file() {
                full
            } else if quick.is_file() {
                quick
            } else {
                anyhow::bail!("в каталоге {} нет ob2h.db / ob2h-quick.db", path.display());
            }
        } else {
            path.to_path_buf()
        };

        let conn = rusqlite::Connection::open_with_flags(
            &db_file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        let integrity: String = match conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)) {
            Ok(v) => v,
            Err(e) => format!("ОШИБКА: {e}"),
        };

        let mut report = format!("файл: {}\nintegrity_check: {integrity}", db_file.display());
        for table in ["memories", "graph_nodes", "memory_links", "chunks", "kv"] {
            let present: bool = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if !present {
                continue;
            }
            let backup_count: i64 =
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            let live_count: i64 = self.db.with_conn(|conn| {
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            })?;
            let mark = if backup_count == live_count { "✓" } else { "⚠ расхождение" };
            report.push_str(&format!("\n{table}: backup={backup_count} live={live_count} {mark}"));
        }
        Ok(report)
    }

    /// Ротация раздельная: full и quick считаются по своим лимитам.
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
        for (is_quick, keep) in [
            (false, self.settings.backup_keep_full),
            (true, self.settings.backup_keep_quick),
        ] {
            let group: Vec<&PathBuf> = dirs
                .iter()
                .filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    name.starts_with("quick-") == is_quick
                })
                .collect();
            if group.len() > keep {
                let to_remove = group.len() - keep;
                for p in &group[..to_remove] {
                    let _ = fs::remove_dir_all(p);
                    removed += 1;
                }
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

    fn copy_workspace(&self, target: &Path) -> anyhow::Result<()> {
        let workspace_src = self.settings.workspace_dir();
        let workspace_dest = target.join("workspace");
        if workspace_src.exists() {
            copy_dir_all(&workspace_src, &workspace_dest)?;
        }
        Ok(())
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
