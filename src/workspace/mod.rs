//! Файловый воркспейс: MEMORY.md, SOUL.md, USER.md, history.jsonl, daily/*.jsonl.

pub mod git;

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use chrono::Utc;
use serde::{Deserialize, Serialize};

pub use git::GitStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub cursor: i64,
    pub timestamp: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyLogEntry {
    pub timestamp: String,
    pub user_text: String,
    pub assistant_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        let p = root.as_ref().to_path_buf();
        let _ = fs::create_dir_all(&p);
        let _ = fs::create_dir_all(p.join("memory"));
        let _ = fs::create_dir_all(p.join("daily"));
        Self { root: p }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn memory_dir(&self) -> PathBuf {
        self.root.join("memory")
    }

    pub fn daily_dir(&self) -> PathBuf {
        self.root.join("daily")
    }

    pub fn resolve_file(&self, name: &str) -> PathBuf {
        match name {
            "memory" | "memory.md" | "MEMORY.md" => self.memory_dir().join("MEMORY.md"),
            "soul" | "soul.md" | "SOUL.md" => self.root.join("SOUL.md"),
            "user" | "user.md" | "USER.md" => self.root.join("USER.md"),
            "history" | "history.jsonl" => self.memory_dir().join("history.jsonl"),
            _ => self.root.join(name),
        }
    }

    pub fn read_file(&self, name: &str) -> anyhow::Result<String> {
        let path = self.resolve_file(name);
        if !path.exists() {
            return Ok(String::new());
        }
        Ok(fs::read_to_string(path)?)
    }

    pub fn write_file(&self, name: &str, content: &str) -> anyhow::Result<()> {
        let path = self.resolve_file(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        Ok(())
    }

    pub fn log_daily_session(
        &self,
        user_text: &str,
        assistant_text: &str,
        meta: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        let date_str = now.format("%Y-%m-%d").to_string();
        let file_path = self.daily_dir().join(format!("{date_str}.jsonl"));

        let entry = DailyLogEntry {
            timestamp: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            user_text: user_text.to_string(),
            assistant_text: assistant_text.to_string(),
            meta,
        };

        let json_line = serde_json::to_string(&entry)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)?;
        writeln!(file, "{json_line}")?;
        Ok(())
    }

    pub fn append_history(&self, content: &str) -> anyhow::Result<i64> {
        let history_file = self.memory_dir().join("history.jsonl");
        let last_cursor = self.get_cursor()?.unwrap_or(0);
        let next_cursor = last_cursor + 1;

        let entry = HistoryEntry {
            cursor: next_cursor,
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            content: content.to_string(),
        };

        let json_line = serde_json::to_string(&entry)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(history_file)?;
        writeln!(file, "{json_line}")?;

        self.set_cursor(next_cursor)?;
        Ok(next_cursor)
    }

    pub fn read_history_from_cursor(
        &self,
        from_cursor: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<HistoryEntry>> {
        let history_file = self.memory_dir().join("history.jsonl");
        if !history_file.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(history_file)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line) {
                if entry.cursor > from_cursor {
                    entries.push(entry);
                    if entries.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(entries)
    }

    pub fn get_cursor(&self) -> anyhow::Result<Option<i64>> {
        let path = self.memory_dir().join(".cursor");
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        Ok(content.trim().parse::<i64>().ok())
    }

    pub fn set_cursor(&self, cursor: i64) -> anyhow::Result<()> {
        let path = self.memory_dir().join(".cursor");
        fs::write(path, cursor.to_string())?;
        Ok(())
    }

    pub fn get_dream_cursor(&self) -> anyhow::Result<Option<i64>> {
        let path = self.memory_dir().join(".dream_cursor");
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        Ok(content.trim().parse::<i64>().ok())
    }

    pub fn set_dream_cursor(&self, cursor: i64) -> anyhow::Result<()> {
        let path = self.memory_dir().join(".dream_cursor");
        fs::write(path, cursor.to_string())?;
        Ok(())
    }

    pub fn compact_history(&self, max_entries: usize) -> anyhow::Result<()> {
        let history_file = self.memory_dir().join("history.jsonl");
        if !history_file.exists() {
            return Ok(());
        }

        let file = fs::File::open(&history_file)?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().flatten().collect();

        if lines.len() <= max_entries {
            return Ok(());
        }

        let keep = &lines[lines.len() - max_entries..];
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&history_file)?;

        for line in keep {
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}
