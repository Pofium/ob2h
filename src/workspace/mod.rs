//! Файловый воркспейс: MEMORY.md, SOUL.md, USER.md, history.jsonl, daily/*.jsonl.

pub mod git;

use std::collections::HashMap;
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

    pub fn resolve_file(&self, name: &str) -> anyhow::Result<PathBuf> {
        match name {
            "memory" | "memory.md" | "MEMORY.md" => Ok(self.memory_dir().join("MEMORY.md")),
            "soul" | "soul.md" | "SOUL.md" => Ok(self.root.join("SOUL.md")),
            "user" | "user.md" | "USER.md" => Ok(self.root.join("USER.md")),
            "history" | "history.jsonl" => Ok(self.memory_dir().join("history.jsonl")),
            _ => safe_join(&self.root, name),
        }
    }

    pub fn read_file(&self, name: &str) -> anyhow::Result<String> {
        let path = self.resolve_file(name)?;
        if !path.exists() {
            return Ok(String::new());
        }
        Ok(fs::read_to_string(path)?)
    }

    pub fn write_file(&self, name: &str, content: &str) -> anyhow::Result<()> {
        let path = self.resolve_file(name)?;
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

    /// Ф35.4: daily-логи старше `retention_days` упаковываются в
    /// `archive/YYYY-MM.jsonl.gz` — **архивация, не удаление**: строки целиком
    /// переезжают в месячный сжатый архив, исходный файл после успешной упаковки
    /// удаляется. Свежие логи и файлы с именами не вида `YYYY-MM-DD.jsonl`
    /// (чужие писатели) не трогаются. Идемпотентно: повторный вызов — no-op.
    pub fn archive_old_logs(&self, retention_days: u32) -> anyhow::Result<Vec<String>> {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64))
            .format("%Y-%m-%d")
            .to_string();
        let archive_dir = self.root.join("archive");
        let mut archived = Vec::new();

        let entries = match fs::read_dir(self.daily_dir()) {
            Ok(e) => e,
            Err(_) => return Ok(archived),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // только канонические daily-логи log_daily_session: строго YYYY-MM-DD.jsonl
            let stem = match name.strip_suffix(".jsonl") {
                Some(s) if s.len() == 10 => s.to_string(),
                _ => continue,
            };
            if chrono::NaiveDate::parse_from_str(&stem, "%Y-%m-%d").is_err() {
                continue;
            }
            // лексикографическое сравнение дат YYYY-MM-DD = хронологическое
            if stem.as_str() >= cutoff.as_str() {
                continue;
            }

            let month = &stem[..7];
            fs::create_dir_all(&archive_dir)?;
            let target = archive_dir.join(format!("{month}.jsonl.gz"));

            // gzip дописывать нельзя — распаковываем существующий месячный архив,
            // добавляем строки дня и упаковываем заново
            let mut buf: Vec<u8> = Vec::new();
            if target.exists() {
                let f = fs::File::open(&target)?;
                let mut dec = flate2::read::GzDecoder::new(f);
                std::io::Read::read_to_end(&mut dec, &mut buf)?;
            }
            buf.extend_from_slice(&fs::read(&path)?);

            let f = fs::File::create(&target)?;
            let mut enc = GzEncoder::new(f, Compression::default());
            enc.write_all(&buf)?;
            enc.finish()?;

            fs::remove_file(&path)?;
            archived.push(name);
        }
        Ok(archived)
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

    /// Ф78.2: дрим-курсор daily-логов — JSON-карта «имя файла → прочитано строк».
    /// Дрим читает daily-логи напрямую, не дожидаясь консолидации в history.jsonl
    /// (порог токен-бюджета консолидатора при малых сессиях не достигается неделями).
    pub fn get_daily_dream_cursor(&self) -> anyhow::Result<HashMap<String, usize>> {
        let path = self.memory_dir().join(".daily_dream_cursor");
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content).unwrap_or_default())
    }

    pub fn set_daily_dream_cursor(&self, offsets: &HashMap<String, usize>) -> anyhow::Result<()> {
        let path = self.memory_dir().join(".daily_dream_cursor");
        fs::write(path, serde_json::to_string(offsets)?)?;
        Ok(())
    }

    /// Ф78.2: новые daily-записи после сохранённых оффсетов, максимум `limit`,
    /// файлы по алфавиту имён = хронологически (`YYYY-MM-DD.jsonl`, канонические
    /// логи log_daily_session; чужие файлы, как и в archive_old_logs, не трогаем).
    /// Записи возвращаются как HistoryEntry с cursor=0 — они НЕ двигают
    /// .dream_cursor (историю представляет history.jsonl). Второй элемент —
    /// карта оффсетов для сохранения ПОСЛЕ успешной обработки: оффсеты
    /// нетронутых файлов сохраняются, файлы-призраки вычищаются.
    pub fn read_daily_from_offsets(
        &self,
        offsets: &HashMap<String, usize>,
        limit: usize,
    ) -> anyhow::Result<(Vec<HistoryEntry>, HashMap<String, usize>)> {
        let mut records = Vec::new();

        let mut files: Vec<(String, PathBuf)> = match fs::read_dir(self.daily_dir()) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|e| {
                    let path = e.path();
                    if !path.is_file() {
                        return None;
                    }
                    let name = path.file_name()?.to_str()?.to_string();
                    let stem = name.strip_suffix(".jsonl")?;
                    if stem.len() != 10 || chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d").is_err()
                    {
                        return None;
                    }
                    Some((name, path))
                })
                .collect(),
            Err(_) => return Ok((records, HashMap::new())),
        };
        files.sort_by(|a, b| a.0.cmp(&b.0));

        // оффсеты нетронутых существующих файлов сохраняем, призраки вычищаем
        let mut new_offsets: HashMap<String, usize> = offsets
            .iter()
            .filter(|(k, _)| files.iter().any(|(n, _)| n == *k))
            .map(|(k, v)| (k.clone(), *v))
            .collect();

        for (name, path) in files {
            if records.len() >= limit {
                break;
            }
            let consumed_before = offsets.get(&name).copied().unwrap_or(0);
            let file = fs::File::open(&path)?;
            let mut lines = BufReader::new(file).lines();
            let mut consumed = consumed_before;
            for line in lines.by_ref().skip(consumed_before) {
                if records.len() >= limit {
                    break;
                }
                // счётчик двигаем по каждой строке файла (в т.ч. битой) —
                // мусор не должен переобрабатываться в каждом дриме
                consumed += 1;
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(entry) = serde_json::from_str::<DailyLogEntry>(&line) else {
                    continue;
                };
                records.push(HistoryEntry {
                    cursor: 0,
                    timestamp: entry.timestamp,
                    content: format!(
                        "user: {}\nassistant: {}",
                        entry.user_text, entry.assistant_text
                    ),
                });
            }
            new_offsets.insert(name, consumed);
        }
        Ok((records, new_offsets))
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

/// Безопасная сборка пути внутри workspace: только относительные пути из
/// нормальных компонентов. `PathBuf::join` с абсолютным путём подменяет базу,
/// а `..` не нормализуется — без этой проверки workspace_read/write дают
/// произвольное чтение/запись файлов.
fn safe_join(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    use std::path::Component;
    let rel = Path::new(name);
    if rel.is_absolute() {
        anyhow::bail!("путь должен быть относительным внутри workspace: {name:?}");
    }
    let mut normalized = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => normalized.push(c),
            Component::CurDir => {}
            other => anyhow::bail!("недопустимый компонент пути {other:?} в {name:?}"),
        }
    }
    if normalized.as_os_str().is_empty() {
        anyhow::bail!("пустой путь файла");
    }
    Ok(root.join(normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ws() -> Workspace {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ob2h_ws_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        Workspace::new(&dir)
    }

    #[test]
    fn resolve_allows_normal_relative_paths() {
        let ws = temp_ws();
        let p = ws.resolve_file("notes/todo.md").unwrap();
        assert_eq!(p, ws.root().join("notes").join("todo.md"));
        assert!(p.starts_with(ws.root()));
    }

    #[test]
    fn resolve_rejects_parent_traversal() {
        let ws = temp_ws();
        assert!(ws.resolve_file("../escape.txt").is_err());
        assert!(ws.resolve_file("a/../../escape.txt").is_err());
        assert!(ws.resolve_file("..").is_err());
    }

    #[test]
    fn resolve_rejects_absolute_paths() {
        let ws = temp_ws();
        let abs = std::env::temp_dir().join("escape.txt");
        assert!(ws.resolve_file(&abs.to_string_lossy()).is_err());
        // Windows-префикс: на POSIX это относительное имя, но на Windows — абсолютный путь.
        if cfg!(windows) {
            assert!(ws.resolve_file("C:\\Windows\\system32\\config").is_err());
        }
    }

    #[test]
    fn resolve_rejects_empty_path() {
        let ws = temp_ws();
        assert!(ws.resolve_file("").is_err());
        assert!(ws.resolve_file("./.").is_err());
    }

    #[test]
    fn aliases_still_work() {
        let ws = temp_ws();
        let p = ws.resolve_file("memory").unwrap();
        assert_eq!(p, ws.memory_dir().join("MEMORY.md"));
    }

    #[test]
    fn write_read_roundtrip_confined() {
        let ws = temp_ws();
        ws.write_file("sub/notes.md", "привет").unwrap();
        assert_eq!(ws.read_file("sub/notes.md").unwrap(), "привет");
        assert!(ws.root().join("sub").join("notes.md").exists());
    }

    // ---- Ф78.2: daily-курсор дрима ----

    fn write_daily_file(ws: &Workspace, date: &str, entries: &[(&str, &str)]) {
        use std::io::Write;
        let path = ws.daily_dir().join(format!("{date}.jsonl"));
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        for (ts, u) in entries {
            let e = DailyLogEntry {
                timestamp: ts.to_string(),
                user_text: u.to_string(),
                assistant_text: format!("ответ на {u}"),
                meta: None,
            };
            writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }
    }

    #[test]
    fn daily_reader_returns_entries_and_advances_offsets() {
        let ws = temp_ws();
        write_daily_file(&ws, "2026-11-14", &[("2026-11-14T09:00:00Z", "вопрос раз")]);
        write_daily_file(
            &ws,
            "2026-11-15",
            &[("2026-11-15T09:00:00Z", "вопрос два"), ("2026-11-15T10:00:00Z", "вопрос три")],
        );

        let (recs, offs) = ws.read_daily_from_offsets(&HashMap::new(), 10).unwrap();
        assert_eq!(recs.len(), 3, "оба файла, хронологический порядок");
        assert!(recs[0].content.contains("вопрос раз"));
        assert!(recs[0].timestamp.starts_with("2026-11-14"));
        assert_eq!(recs[0].cursor, 0, "daily-записи не двигают .dream_cursor");
        assert_eq!(offs["2026-11-14.jsonl"], 1);
        assert_eq!(offs["2026-11-15.jsonl"], 2);

        // повторный вызов с сохранёнными оффсетами — пусто
        let (recs2, _) = ws.read_daily_from_offsets(&offs, 10).unwrap();
        assert!(recs2.is_empty(), "всё уже потреблено");
    }

    #[test]
    fn daily_reader_partial_limit_keeps_unconsumed() {
        let ws = temp_ws();
        write_daily_file(
            &ws,
            "2026-11-15",
            &[("2026-11-15T09:00:00Z", "а"), ("2026-11-15T10:00:00Z", "б"), ("2026-11-15T11:00:00Z", "в")],
        );

        let (recs, offs) = ws.read_daily_from_offsets(&HashMap::new(), 2).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(offs["2026-11-15.jsonl"], 2, "частичное потребление");

        // хвост дочитывается со второй строки
        let (recs2, offs2) = ws.read_daily_from_offsets(&offs, 10).unwrap();
        assert_eq!(recs2.len(), 1);
        assert!(recs2[0].content.contains("в"));
        assert_eq!(offs2["2026-11-15.jsonl"], 3);
    }

    #[test]
    fn daily_reader_skips_broken_lines_and_foreign_files() {
        let ws = temp_ws();
        use std::io::Write;
        let good = ws.daily_dir().join("2026-11-15.jsonl");
        let mut f = OpenOptions::new().create(true).append(true).open(&good).unwrap();
        writeln!(f, "{{битый json").unwrap();
        writeln!(f).unwrap(); // пустая строка
        let e = DailyLogEntry {
            timestamp: "2026-11-15T12:00:00Z".to_string(),
            user_text: "нормальная".to_string(),
            assistant_text: "запись".to_string(),
            meta: None,
        };
        writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        // чужой файл (не YYYY-MM-DD.jsonl) — как и в archive_old_logs, не трогаем
        ws.write_file("daily/omnes-agent-2026-09-04.jsonl", "{\"x\":1}").unwrap();

        let (recs, offs) = ws.read_daily_from_offsets(&HashMap::new(), 10).unwrap();
        assert_eq!(recs.len(), 1, "битая и пустая строки пропущены, чужой файл не читается");
        assert!(recs[0].content.contains("нормальная"));
        assert_eq!(offs["2026-11-15.jsonl"], 3, "битые строки тоже считаются потреблёнными");
        assert!(!offs.contains_key("omnes-agent-2026-09-04.jsonl"));
    }

    #[test]
    fn daily_reader_prunes_ghost_offsets() {
        let ws = temp_ws();
        let mut stale = HashMap::new();
        stale.insert("2026-01-01.jsonl".to_string(), 5usize); // файла больше нет

        let (recs, offs) = ws.read_daily_from_offsets(&stale, 10).unwrap();
        assert!(recs.is_empty());
        assert!(offs.is_empty(), "оффсеты удалённых файлов вычищаются");
    }

    #[test]
    fn daily_cursor_roundtrip() {
        let ws = temp_ws();
        assert!(ws.get_daily_dream_cursor().unwrap().is_empty());
        let mut m = HashMap::new();
        m.insert("2026-11-15.jsonl".to_string(), 7usize);
        ws.set_daily_dream_cursor(&m).unwrap();
        assert_eq!(ws.get_daily_dream_cursor().unwrap(), m);
    }
}

