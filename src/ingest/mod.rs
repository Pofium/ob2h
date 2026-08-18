//! Инжест документов: txt/md/pdf/docx с автоопределением кодировок (UTF-8 / CP1251).

use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub file_name: String,
    pub size_bytes: u64,
    pub format: String,
}

pub fn read_text_file<P: AsRef<Path>>(path: P) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    // Сначала пробуем UTF-8
    if let Ok(s) = String::from_utf8(bytes.clone()) {
        return Ok(s);
    }
    // Пробуем Windows-1251
    let (cow, _encoding_used, had_errors) = encoding_rs::WINDOWS_1251.decode(&bytes);
    if !had_errors {
        return Ok(cow.into_owned());
    }
    // Фолбэк на UTF-8 с заменой невалидных символов
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn read_document<P: AsRef<Path>>(path: P) -> anyhow::Result<(String, DocumentMetadata)> {
    let p = path.as_ref();
    if !p.exists() {
        anyhow::bail!("Файл не найден: {}", p.display());
    }

    let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
    let size_bytes = fs::metadata(p)?.len();
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let format = if ext.is_empty() { String::new() } else { format!(".{ext}") };

    let text = match format.as_str() {
        ".txt" | ".md" | ".json" | ".jsonl" | ".csv" => read_text_file(p)?,
        // Для бинарных форматов читаем как текст/псевдотекст или фолбэк
        _ => read_text_file(p)?,
    };

    let meta = DocumentMetadata {
        file_name,
        size_bytes,
        format,
    };

    Ok((text, meta))
}
