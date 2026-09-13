//! Управление Git-историей для MD-файлов воркспейса (SOUL.md, USER.md, memory/MEMORY.md).

use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::warn;

pub const TRACKED_FILES: &[&str] = &["SOUL.md", "USER.md", "memory/MEMORY.md"];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GitCommitEntry {
    pub sha: String,
    pub date: String,
    pub message: String,
}

pub struct GitStore {
    root: PathBuf,
}

impl GitStore {
    pub fn new<P: AsRef<Path>>(workspace_root: P) -> Self {
        Self {
            root: workspace_root.as_ref().to_path_buf(),
        }
    }

    fn run_git(&self, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .ok()?;

        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            warn!("git {:?}: {}", args, err.trim());
            None
        }
    }

    pub fn ensure_repo(&self) -> bool {
        if self.root.join(".git").exists() {
            return true;
        }
        let _ = self.run_git(&["init", "-q", "-b", "main"]);
        let _ = self.run_git(&["config", "user.email", "ob2h-dream@local"]);
        let _ = self.run_git(&["config", "user.name", "OB2H Dream"]);
        self.root.join(".git").exists()
    }

    pub fn auto_commit(&self, message: &str) -> Option<String> {
        if !self.ensure_repo() {
            return None;
        }

        let mut existing_files = Vec::new();
        for file in TRACKED_FILES {
            if self.root.join(file).exists() {
                existing_files.push(*file);
            }
        }

        if existing_files.is_empty() {
            return None;
        }

        let mut add_args = vec!["add", "--"];
        add_args.extend_from_slice(&existing_files);
        self.run_git(&add_args)?;

        let mut status_args = vec!["status", "--porcelain", "--"];
        status_args.extend_from_slice(&existing_files);
        let status = self.run_git(&status_args)?;
        if status.is_empty() {
            return None;
        }

        self.run_git(&["commit", "-q", "-m", message])?;
        self.run_git(&["rev-parse", "--short", "HEAD"])
    }

    pub fn log(&self, limit: usize) -> Vec<GitCommitEntry> {
        if !self.ensure_repo() {
            return Vec::new();
        }

        let limit_str = limit.to_string();
        let format_arg = "--format=%h\t%ad\t%s";
        let date_arg = "--date=format:%Y-%m-%d %H:%M";
        let out = match self.run_git(&["log", "-n", &limit_str, format_arg, date_arg]) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let mut entries = Vec::new();
        for line in out.lines() {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.len() == 3 {
                entries.push(GitCommitEntry {
                    sha: parts[0].to_string(),
                    date: parts[1].to_string(),
                    message: parts[2].to_string(),
                });
            }
        }
        entries
    }

    pub fn restore(&self, commit_ref: &str) -> String {
        if !self.ensure_repo() {
            return "git_unavailable".to_string();
        }

        let mut existing_files = Vec::new();
        for file in TRACKED_FILES {
            if self.root.join(file).exists() {
                existing_files.push(*file);
            }
        }

        if existing_files.is_empty() {
            return "nothing_to_restore".to_string();
        }

        let mut checkout_args = vec!["checkout", commit_ref, "--"];
        checkout_args.extend_from_slice(&existing_files);
        match self.run_git(&checkout_args) {
            Some(_) => format!("restored from {commit_ref}"),
            None => format!("restore_failed: {commit_ref}"),
        }
    }

    /// Текущий HEAD (короткий sha) — Ф30: фиксация состояния workspace до дрима.
    pub fn head(&self) -> Option<String> {
        if !self.ensure_repo() {
            return None;
        }
        self.run_git(&["rev-parse", "--short", "HEAD"])
    }

    /// Ф30: workspace-инвариант — файл потерял больше max_drop доли непустых строк
    /// относительно коммита commit_ref. Возвращает имя испорченного файла или None.
    /// Ловит «дрим съел память» — то, что bench по БД увидеть не может.
    pub fn file_shrunk_beyond(&self, commit_ref: &str, max_drop: f64) -> Option<String> {
        if !self.ensure_repo() {
            return None;
        }
        for file in TRACKED_FILES {
            let before = match self.run_git(&["show", &format!("{commit_ref}:{file}")]) {
                Some(b) => b,
                None => continue, // файла нет в коммите или git недоступен — не наш случай
            };
            let before_count = before.lines().filter(|l| !l.trim().is_empty()).count();
            if before_count == 0 {
                continue;
            }
            let after = std::fs::read_to_string(self.root.join(file)).unwrap_or_default();
            let after_count = after.lines().filter(|l| !l.trim().is_empty()).count();
            if (after_count as f64) < before_count as f64 * (1.0 - max_drop) {
                return Some(file.to_string());
            }
        }
        None
    }
}
