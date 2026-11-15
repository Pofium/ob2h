//! AST-aware чанкинг кода для векторизации (Ф72.2a).
//! Нарезка строго по границам AST-узлов (`line_start..=line_end`) с заголовком
//! `// File: <путь> | Scope: <контекст>`, без разрыва строк и без векторизации stub-файлов (70.15).

use std::path::Path;

/// Версия схемы чанкинга (входит в `EmbeddingFingerprint`, Ф72.3).
pub const CHUNKER_VERSION: &str = "ast_v1";

/// Максимальный размер одного кодового чанка в символах (безопасно для 512 токенов).
pub const DEFAULT_CHUNK_MAX_CHARS: usize = 1500;

/// Сформировать AST-aware чанк с обязательным контекстным заголовком.
pub fn format_ast_chunk(
    file_path: &str,
    scope: &str,
    code_body: &str,
    max_chars: usize,
) -> String {
    let header = format!("// File: {file_path} | Scope: {scope}");
    let body = code_body.trim();
    if body.is_empty() {
        return header;
    }

    let mut out = String::with_capacity(header.len() + 1 + body.len().min(max_chars));
    out.push_str(&header);
    out.push('\n');

    let mut used = 0usize;
    for line in body.lines() {
        let line_chars = line.chars().count();
        if used > 0 && used + 1 + line_chars > max_chars {
            break;
        }
        if used > 0 {
            out.push('\n');
            used += 1;
        }
        if line_chars > max_chars && used == 0 {
            let truncated: String = line.chars().take(max_chars).collect();
            out.push_str(&truncated);
            break;
        }
        out.push_str(line);
        used += line_chars;
    }

    out
}

/// Извлечь тело узла по `line_start..=line_end` (1-based) из файла и обернуть в AST-чанк.
/// При отсутствии файла или границ строк формирует компактный чанк из описания узла.
pub fn extract_node_span_chunk(
    project_root: Option<&Path>,
    file_path: Option<&str>,
    node_type: &str,
    label: &str,
    description: Option<&str>,
    line_start: Option<i64>,
    line_end: Option<i64>,
) -> String {
    let fpath = file_path.unwrap_or("-");
    let scope = match description.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => format!("{node_type} {label} ({d})"),
        None => format!("{node_type} {label}"),
    };

    if let (Some(root), Some(rel), Some(ls)) = (project_root, file_path, line_start) {
        if ls >= 1 {
            let full = root.join(rel);
            if let Ok(content) = std::fs::read_to_string(&full) {
                let start_idx = (ls - 1) as usize;
                let end_idx = line_end
                    .map(|le| (le.max(ls) - 1) as usize)
                    .unwrap_or(start_idx.saturating_add(40));
                let lines: Vec<&str> = content.lines().collect();
                if start_idx < lines.len() {
                    let slice_end = (end_idx + 1).min(lines.len());
                    let body = lines[start_idx..slice_end].join("\n");
                    if !body.trim().is_empty() {
                        return format_ast_chunk(fpath, &scope, &body, DEFAULT_CHUNK_MAX_CHARS);
                    }
                }
            }
        }
    }

    format_ast_chunk(fpath, &scope, description.unwrap_or(""), DEFAULT_CHUNK_MAX_CHARS)
}

/// Нарезка файла окнами фиксированного размера (для baseline-сравнения в bench 72.2a).
pub fn fixed_window_chunks(content: &str, window_lines: usize, overlap: usize) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() || window_lines == 0 {
        return Vec::new();
    }
    let step = window_lines.saturating_sub(overlap).max(1);
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + window_lines).min(lines.len());
        let chunk = lines[start..end].join("\n");
        if !chunk.trim().is_empty() {
            chunks.push(chunk);
        }
        if end == lines.len() {
            break;
        }
        start += step;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_format_ast_chunk_header_and_line_boundary() {
        let chunk = format_ast_chunk(
            "src/auth/jwt.rs",
            "Function verify_token",
            "pub fn verify_token(tok: &str) -> bool {\n    !tok.is_empty()\n}\n",
            55,
        );
        assert!(chunk.starts_with("// File: src/auth/jwt.rs | Scope: Function verify_token\n"));
        assert!(chunk.contains("pub fn verify_token"));
        assert!(!chunk.contains("!tok.is_empty()\n}"));
    }

    #[test]
    fn test_extract_node_span_chunk_reads_ast_span() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("lib.rs");
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(f, "// comment").unwrap();
        writeln!(f, "pub fn add_numbers(a: i32, b: i32) -> i32 {{").unwrap();
        writeln!(f, "    a + b").unwrap();
        writeln!(f, "}}").unwrap();

        let chunk = extract_node_span_chunk(
            Some(tmp.path()),
            Some("lib.rs"),
            "Function",
            "add_numbers",
            Some("Сложение двух чисел"),
            Some(2),
            Some(4),
        );
        assert!(chunk.starts_with("// File: lib.rs | Scope: Function add_numbers (Сложение двух чисел)\n"));
        assert!(chunk.contains("pub fn add_numbers"));
        assert!(chunk.contains("a + b"));
        assert!(!chunk.contains("// comment"));
    }

    #[test]
    fn test_ast_chunk_beats_fixed_window_on_symbol_recall() {
        // Фиксированное окно (4 строки) режет тело функции пополам без имени/файла,
        // тогда как AST-чанк сохраняет файл, scope и полное тело — выше Recall@10.
        let src = "use std::io;\n\n\n\npub fn parse_config_file() {\n    // line 1\n    // line 2\n    // line 3\n    let timeout_ms = 5000;\n    let retries = 3;\n}\n";
        let fixed = fixed_window_chunks(src, 4, 0);
        // В fixed[2] ("let timeout_ms = 5000; ...") нет ни имени функции, ни пути файла
        assert!(!fixed[2].contains("parse_config_file"));

        let ast = format_ast_chunk(
            "src/config.rs",
            "Function parse_config_file",
            &src.lines().skip(4).collect::<Vec<_>>().join("\n"),
            DEFAULT_CHUNK_MAX_CHARS,
        );
        assert!(ast.contains("src/config.rs"));
        assert!(ast.contains("parse_config_file"));
        assert!(ast.contains("timeout_ms"));
    }
}
