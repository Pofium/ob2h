//! Высокопроизводительный статический AST/код-парсер репозиториев (Rust, Python, TS/JS, Go, SQL, C/C++).
//! Работает полностью детерминированно и локально, без вызовов LLM и без расхода токенов.

use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Извлеченный узел графа кода.
#[derive(Debug, Clone)]
pub struct AstNode {
    pub node_id: String,
    pub label: String,
    pub node_type: String, // 'Module' | 'Struct' | 'Class' | 'Interface' | 'Function' | 'Table' | 'File'
    pub description: String,
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
}

/// Извлеченное ребро графа кода.
#[derive(Debug, Clone)]
pub struct AstEdge {
    pub source_node_id: String,
    pub target_node_id: String,
    pub label: String, // 'IMPORTS' | 'CALLS' | 'IMPLEMENTS' | 'DEFINES' | 'DEPENDS_ON' | 'FOREIGN_KEY_TO'
    pub weight: f64,
    pub context: String,
    /// Ф39.2 (M8): происхождение ребра — EXTRACTED (обычный скан), RESOLVED
    /// (type-pass Ф39.1), INFERRED (эвристики Ф40). Старые рёбра в БД хранят
    /// своё значение ('ast'/'manual') и читаются как есть.
    pub provenance: String,
}

/// Ф39.1: замеченное место вызова (для последующего резолва type-pass'ом).
#[derive(Debug, Clone)]
pub struct CallSite {
    pub rel_path: String,
    pub line: usize,
    pub name: String,
}

/// Ф39.1: импорт с элементом и алиасом (use/import ... as ...).
#[derive(Debug, Clone)]
pub struct ImportRef {
    pub rel_path: String,
    pub module: String,
    pub item: Option<String>,
    pub alias: Option<String>,
}

/// Результат AST-сканирования кодовой базы.
#[derive(Debug, Clone, Default)]
pub struct AstScanResult {
    pub files_scanned: usize,
    pub lines_total: usize,
    pub nodes: Vec<AstNode>,
    pub edges: Vec<AstEdge>,
    pub file_hashes: HashMap<String, String>,
    pub file_meta: HashMap<String, (usize, usize)>, // rel_path -> (file_size, lines_count)
    /// Ф39.1: места вызовов (Rust/Python) для type-pass.
    pub call_sites: Vec<CallSite>,
    /// Ф39.1: импорты с алиасами (Rust/Python) для type-pass.
    pub imports: Vec<ImportRef>,
}

/// Ключевые слова, которые не считаются вызовами (Rust).
const RUST_CALL_SKIP: &[&str] = &[
    "if", "for", "while", "match", "loop", "return", "unsafe", "fn", "let", "struct", "impl",
    "mod", "use", "pub", "crate", "self", "super", "as", "in", "else", "await", "move", "box",
    "where", "type", "dyn", "ref", "const", "static", "enum", "trait",
];

/// Ключевые слова, которые не считаются вызовами (Python).
const PY_CALL_SKIP: &[&str] = &[
    "if", "for", "while", "return", "def", "class", "import", "from", "with", "elif", "else",
    "except", "lambda", "yield", "assert", "del", "in", "is", "not", "and", "or", "await", "async",
    "raise", "global", "nonlocal",
];

/// AST-экстрактор для проектов.
#[allow(dead_code)]
pub struct AstCodeExtractor {
    rust_fn_re: Regex,
    rust_struct_re: Regex,
    rust_trait_re: Regex,
    rust_impl_re: Regex,
    rust_use_re: Regex,

    py_class_re: Regex,
    py_fn_re: Regex,
    py_import_re: Regex,

    ts_class_re: Regex,
    ts_interface_re: Regex,
    ts_fn_re: Regex,
    ts_import_re: Regex,

    go_fn_re: Regex,
    go_struct_re: Regex,
    go_import_re: Regex,

    sql_table_re: Regex,
    sql_fk_re: Regex,

    php_use_re: Regex,
    php_class_re: Regex,
    php_interface_re: Regex,
    php_trait_re: Regex,
    php_fn_re: Regex,

    /// Ф39.1: общий паттерн места вызова `name(` (Rust/Python type-pass).
    call_re: Regex,

    /// Ф40.2: маршруты axum/actix `#[get("/path")]`.
    rust_route_re: Regex,
    /// Ф40.2: маршруты FastAPI/Flask `@app.get("/path")`.
    py_route_re: Regex,
    /// Ф40.2: SQLAlchemy `__tablename__ = "x"`.
    py_tablename_re: Regex,

    dart_import_re: Regex,
    dart_class_re: Regex,
    dart_mixin_re: Regex,
    dart_fn_re: Regex,

    java_package_re: Regex,
    java_import_re: Regex,
    java_class_re: Regex,
    java_interface_re: Regex,
    java_fn_re: Regex,
}

impl Default for AstCodeExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl AstCodeExtractor {
    pub fn new() -> Self {
        Self {
            // Rust patterns
            rust_fn_re: Regex::new(r"(?m)^\s*(?:pub(?:\([^)]+\))?\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)\s*(?:<[^>]+>)?\s*\(([^)]*)\)(?:\s*->\s*([^{]+))?").unwrap(),
            rust_struct_re: Regex::new(r"(?m)^\s*(?:pub(?:\([^)]+\))?\s+)?(?:struct|enum)\s+([a-zA-Z0-9_]+)").unwrap(),
            rust_trait_re: Regex::new(r"(?m)^\s*(?:pub(?:\([^)]+\))?\s+)?trait\s+([a-zA-Z0-9_]+)").unwrap(),
            rust_impl_re: Regex::new(r"(?m)^\s*impl(?:<[^>]+>)?\s+(?:([a-zA-Z0-9_:]+)\s+for\s+)?([a-zA-Z0-9_:]+)").unwrap(),
            rust_use_re: Regex::new(r"(?m)^\s*use\s+([^;]+);").unwrap(),

            // Ф39.1: место вызова `name(` (для type-pass)
            call_re: Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap(),

            // Ф40.2: framework-эвристики (ROUTE / QUERIES_TABLE, provenance=INFERRED)
            rust_route_re: Regex::new(r#"#\[\s*(get|post|put|delete|patch|route)\s*\(\s*"([^"]+)""#).unwrap(),
            py_route_re: Regex::new(r#"(?:app|router|api)\.(get|post|put|delete|patch)\s*\(\s*["']([^"']+)["']"#).unwrap(),
            py_tablename_re: Regex::new(r#"__tablename__\s*=\s*["'](\w+)["']"#).unwrap(),

            // Python patterns
            py_class_re: Regex::new(r"(?m)^\s*class\s+([a-zA-Z0-9_]+)(?:\(([^)]*)\))?:").unwrap(),
            py_fn_re: Regex::new(r"(?m)^\s*(?:async\s+)?def\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)(?:\s*->\s*([^:]+))?:").unwrap(),
            py_import_re: Regex::new(r"(?m)^\s*(?:from\s+([a-zA-Z0-9_.]+)\s+import\s+([^#\n]+)|import\s+([^#\n]+))").unwrap(),

            // TS/JS patterns
            ts_class_re: Regex::new(r"(?m)^\s*(?:export\s+)?(?:default\s+)?class\s+([a-zA-Z0-9_]+)(?:\s+extends\s+([a-zA-Z0-9_]+))?(?:\s+implements\s+([a-zA-Z0-9_, ]+))?").unwrap(),
            ts_interface_re: Regex::new(r"(?m)^\s*(?:export\s+)?(?:interface|type)\s+([a-zA-Z0-9_]+)").unwrap(),
            ts_fn_re: Regex::new(r"(?m)^\s*(?:export\s+)?(?:async\s+)?(?:function\s+([a-zA-Z0-9_]+)|(?:const|let|var)\s+([a-zA-Z0-9_]+)\s*=\s*(?:async\s+)?\([^)]*\)\s*=>)").unwrap(),
            ts_import_re: Regex::new(r#"(?m)^\s*import\s+(?:(?:\{([^}]+)\}|\*\s+as\s+([a-zA-Z0-9_]+)|([a-zA-Z0-9_]+))\s+from\s+)?['"]([^'"]+)['"]"#).unwrap(),

            // Go patterns
            go_fn_re: Regex::new(r"(?m)^\s*func\s+(?:\((?:[^)]+)\)\s+)?([a-zA-Z0-9_]+)\s*\(").unwrap(),
            go_struct_re: Regex::new(r"(?m)^\s*type\s+([a-zA-Z0-9_]+)\s+(?:struct|interface)").unwrap(),
            go_import_re: Regex::new(r#"(?m)^\s*(?:import\s+['"]([^'"]+)['"]|import\s*\(([^)]+)\))"#).unwrap(),

            // SQL patterns
            sql_table_re: Regex::new(r"(?i)CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([a-zA-Z0-9_`\[\]]+)").unwrap(),
            sql_fk_re: Regex::new(r"(?i)REFERENCES\s+([a-zA-Z0-9_`\[\]]+)\s*\(([a-zA-Z0-9_`\[\]]+)\)").unwrap(),

            // PHP patterns
            php_use_re: Regex::new(r"(?m)^\s*use\s+(?:function\s+|const\s+)?([a-zA-Z0-9_\\]+)(?:\s+as\s+([a-zA-Z0-9_]+))?;").unwrap(),
            php_class_re: Regex::new(r"(?m)^\s*(?:(?:final|abstract|readonly)\s+)*class\s+([a-zA-Z0-9_]+)(?:\s+extends\s+([a-zA-Z0-9_\\]+))?(?:\s+implements\s+([a-zA-Z0-9_\\,\s]+))?").unwrap(),
            php_interface_re: Regex::new(r"(?m)^\s*interface\s+([a-zA-Z0-9_]+)(?:\s+extends\s+([a-zA-Z0-9_\\,\s]+))?").unwrap(),
            php_trait_re: Regex::new(r"(?m)^\s*trait\s+([a-zA-Z0-9_]+)").unwrap(),
            php_fn_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|final|abstract)\s+)*function\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)").unwrap(),

            // Dart patterns
            dart_import_re: Regex::new(r#"(?m)^\s*(?:import|export)\s+['"]([^'"]+)['"](?:\s+as\s+([a-zA-Z0-9_]+))?"#).unwrap(),
            dart_class_re: Regex::new(r"(?m)^\s*(?:(?:abstract|base|final|interface|sealed)\s+)*class\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+extends\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?)?(?:\s+with\s+([a-zA-Z0-9_,\s]+))?(?:\s+implements\s+([a-zA-Z0-9_,\s]+))?").unwrap(),
            dart_mixin_re: Regex::new(r"(?m)^\s*mixin\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+on\s+([a-zA-Z0-9_,\s]+))?").unwrap(),
            dart_fn_re: Regex::new(r"(?m)^\s*(?:(?:static|async|void|[a-zA-Z0-9_<>?]+)\s+)+([a-zA-Z0-9_]+)\s*\(([^)]*)\)\s*(?:async\*?|=>|\{)").unwrap(),

            // Java patterns
            java_package_re: Regex::new(r"(?m)^\s*package\s+([a-zA-Z0-9_.]+);").unwrap(),
            java_import_re: Regex::new(r"(?m)^\s*import\s+(?:static\s+)?([a-zA-Z0-9_.*]+);").unwrap(),
            java_class_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|final|abstract|sealed|non-sealed)\s+)*(?:class|enum|record)\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+extends\s+([a-zA-Z0-9_.]+)(?:<[^>]+>)?)?(?:\s+implements\s+([a-zA-Z0-9_.,\s]+))?").unwrap(),
            java_interface_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|sealed|non-sealed)\s+)*interface\s+([a-zA-Z0-9_]+)(?:<[^>]+>)?(?:\s+extends\s+([a-zA-Z0-9_.,\s]+))?").unwrap(),
            java_fn_re: Regex::new(r"(?m)^\s*(?:(?:public|protected|private|static|final|abstract|synchronized|native|default)\s+)+(?:<[^>]+>\s+)?([a-zA-Z0-9_<>\[\]]+)\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)").unwrap(),
        }
    }

    /// Вычисляет SHA256 хэш файла.
    pub fn file_sha256(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hex::encode(hasher.finalize())
    }

    /// Сканирует кодовую базу репозитория по указанному пути.
    pub fn scan_directory(
        &self,
        root: &Path,
        known_hashes: Option<&HashMap<String, String>>,
    ) -> AstScanResult {
        let mut result = AstScanResult::default();
        let mut files_to_scan = Vec::new();

        self.collect_files(root, &mut files_to_scan);

        for (rel_path, abs_path) in files_to_scan {
            if let Ok(bytes) = fs::read(&abs_path) {
                let hash = Self::file_sha256(&bytes);
                let file_size = bytes.len();
                result.file_hashes.insert(rel_path.clone(), hash.clone());

                if let Some(known) = known_hashes {
                    if let Some(prev_hash) = known.get(&rel_path) {
                        if prev_hash == &hash {
                            continue; // Файл не изменился
                        }
                    }
                }

                if let Ok(content) = String::from_utf8(bytes) {
                    let lines_count = content.lines().count();
                    result.files_scanned += 1;
                    result.lines_total += lines_count;
                    result
                        .file_meta
                        .insert(rel_path.clone(), (file_size, lines_count));
                    self.parse_file(&rel_path, &content, &mut result);
                }
            }
        }

        result
    }

    fn collect_files(&self, root: &Path, out: &mut Vec<(String, PathBuf)>) {
        let mut builder = ignore::WalkBuilder::new(root);
        builder
            .hidden(true)
            .parents(true)
            .ignore(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .add_custom_ignore_filename(".gitignore");

        for entry in builder.build().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy();

            // Пропускаем служебные и тяжёлые каталоги сборки/зависимостей
            if name == "target"
                || name == "node_modules"
                || name == "dist"
                || name == "build"
                || name == "venv"
                || name == ".venv"
                || name == "__pycache__"
                || name == "vendor"
                || name == "data"
            {
                continue;
            }

            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if matches!(
                        ext_lower.as_str(),
                        "rs" | "py"
                            | "ts"
                            | "tsx"
                            | "js"
                            | "jsx"
                            | "go"
                            | "sql"
                            | "c"
                            | "cpp"
                            | "h"
                            | "hpp"
                            | "php"
                            | "dart"
                            | "java"
                    ) {
                        if let Ok(rel) = path.strip_prefix(root) {
                            let rel_str = rel.to_string_lossy().replace('\\', "/");
                            // Дополнительная проверка, чтобы не попасть в поддиректории исключений
                            if !rel_str.contains("/target/")
                                && !rel_str.contains("/node_modules/")
                                && !rel_str.contains("/venv/")
                                && !rel_str.contains("/.venv/")
                                && !rel_str.contains("/vendor/")
                                && !rel_str.contains("/data/")
                            {
                                out.push((rel_str, path.to_path_buf()));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Парсит содержимое отдельного файла в узлы и ребра AST.
    pub fn parse_file(&self, rel_path: &str, content: &str, out: &mut AstScanResult) {
        let file_node_id = format!("file:{}", rel_path);
        let file_label = rel_path.to_string();

        out.nodes.push(AstNode {
            node_id: file_node_id.clone(),
            label: file_label,
            node_type: "File".to_string(),
            description: format!("Исходный файл проекта: {}", rel_path),
            file_path: rel_path.to_string(),
            line_start: 1,
            line_end: content.lines().count().max(1),
        });

        let ext = Path::new(rel_path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "rs" => self.parse_rust(rel_path, &file_node_id, content, out),
            "py" => self.parse_python(rel_path, &file_node_id, content, out),
            "ts" | "tsx" | "js" | "jsx" => self.parse_ts_js(rel_path, &file_node_id, content, out),
            "go" => self.parse_go(rel_path, &file_node_id, content, out),
            "sql" => self.parse_sql(rel_path, &file_node_id, content, out),
            "php" => self.parse_php(rel_path, &file_node_id, content, out),
            "dart" => self.parse_dart(rel_path, &file_node_id, content, out),
            "java" => self.parse_java(rel_path, &file_node_id, content, out),
            _ => {}
        }
    }

    fn parse_rust(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Извлечение use-импортов
        for cap in self.rust_use_re.captures_iter(content) {
            if let Some(import_path) = cap.get(1) {
                let import_str = import_path.as_str().trim();
                let target_node_id = format!("module:{}", import_str);
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id,
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("use {};", import_str),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 1b. Ф39.1: импорты с алиасами для type-pass
        for cap in self.rust_use_re.captures_iter(content) {
            if let Some(import_path) = cap.get(1) {
                let import_str = import_path.as_str().trim();
                if let Some((module, item, alias)) = split_rust_use(import_str) {
                    out.imports.push(ImportRef {
                        rel_path: rel_path.to_string(),
                        module,
                        item,
                        alias,
                    });
                }
            }
        }

        // 1c. Ф39.1: места вызовов для type-pass
        collect_call_sites(rel_path, &lines, RUST_CALL_SKIP, &self.call_re, out);

        // 2. Извлечение struct/enum
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_struct_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let struct_name = name_match.as_str();
                    let node_id = format!("struct:{}:{}", rel_path, struct_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: struct_name.to_string(),
                        node_type: "Struct".to_string(),
                        description: format!(
                            "Структура/перечисление `{}` в {}",
                            struct_name, rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Извлечение traits
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_trait_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let trait_name = name_match.as_str();
                    let node_id = format!("trait:{}:{}", rel_path, trait_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: trait_name.to_string(),
                        node_type: "Trait".to_string(),
                        description: format!("Трейт `{}` в {}", trait_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 4. Извлечение functions
        // Ф40.2: маршруты axum/actix — `#[get("/path")]` перед fn
        let mut route_by_line: std::collections::HashMap<usize, (String, String)> =
            std::collections::HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_route_re.captures(line) {
                if let (Some(m), Some(p)) = (cap.get(1), cap.get(2)) {
                    route_by_line.insert(i, (m.as_str().to_uppercase(), p.as_str().to_string()));
                }
            }
        }
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.rust_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    let sig = line.trim().trim_end_matches('{').trim();
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Функция `{}` ({}) в {}", fn_name, sig, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: sig.to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    // Ф40.2: ROUTE — framework edge (provenance=INFERRED):
                    // атрибут маршрута на этой или одной из двух предыдущих строк
                    let route = [i.checked_sub(2), i.checked_sub(1), Some(i)]
                        .into_iter()
                        .flatten()
                        .find_map(|k| route_by_line.get(&k).cloned());
                    if let Some((method, path)) = route {
                        out.edges.push(AstEdge {
                            source_node_id: node_id,
                            target_node_id: format!("route:{} {}", method, path),
                            label: "ROUTE".to_string(),
                            weight: 1.0,
                            context: format!("{} {}", method, path),
                            provenance: "INFERRED".to_string(),
                        });
                    }
                }
            }
        }
    }

    fn parse_python(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Извлечение imports
        for cap in self.py_import_re.captures_iter(content) {
            let mod_name = cap
                .get(1)
                .or_else(|| cap.get(3))
                .map(|m| m.as_str().trim())
                .unwrap_or("");
            if !mod_name.is_empty() {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", mod_name),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {}", mod_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 1b. Ф39.1: импорты с алиасами для type-pass
        for cap in self.py_import_re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                // from X import a, b as c
                let module = m.as_str().trim();
                let items = cap.get(2).map(|mm| mm.as_str()).unwrap_or("");
                if module.is_empty() {
                    continue;
                }
                for it in items.split(',') {
                    let it = it.trim();
                    if it.is_empty() {
                        continue;
                    }
                    let (item, alias) = match it.split_once(" as ") {
                        Some((n, a)) => (n.trim().to_string(), Some(a.trim().to_string())),
                        None => (it.to_string(), None),
                    };
                    out.imports.push(ImportRef {
                        rel_path: rel_path.to_string(),
                        module: module.to_string(),
                        item: Some(item),
                        alias,
                    });
                }
            } else if let Some(m) = cap.get(3) {
                // import x.y
                let mod_name = m.as_str().trim();
                if !mod_name.is_empty() {
                    out.imports.push(ImportRef {
                        rel_path: rel_path.to_string(),
                        module: mod_name.to_string(),
                        item: None,
                        alias: None,
                    });
                }
            }
        }

        // 1c. Ф39.1: места вызовов для type-pass
        collect_call_sites(rel_path, &lines, PY_CALL_SKIP, &self.call_re, out);

        // 2. Извлечение classes
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.py_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Базовые классы
                    if let Some(bases_match) = cap.get(2) {
                        for base in bases_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if !base_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("class:{}", base_clean),
                                    label: "INHERITS".to_string(),
                                    weight: 1.0,
                                    context: format!("extends {}", base_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 2b. Ф40.2: QUERIES_TABLE — SQLAlchemy `__tablename__ = "x"` в классе
        let mut last_class: Option<String> = None;
        for line in lines.iter() {
            let trimmed = line.trim_start();
            if let Some(cap) = self.py_class_re.captures(line) {
                if let Some(nm) = cap.get(1) {
                    last_class = Some(format!("class:{}:{}", rel_path, nm.as_str()));
                    continue;
                }
            }
            if let Some(cap) = self.py_tablename_re.captures(line) {
                if let (Some(tbl), Some(cls)) = (cap.get(1), last_class.clone()) {
                    out.edges.push(AstEdge {
                        source_node_id: cls,
                        target_node_id: format!("table:{}", tbl.as_str()),
                        label: "QUERIES_TABLE".to_string(),
                        weight: 1.0,
                        context: format!("__tablename__ = \"{}\"", tbl.as_str()),
                        provenance: "INFERRED".to_string(),
                    });
                }
            }
            let _ = trimmed;
        }

        // 3. Извлечение functions
        // Ф40.2: маршруты FastAPI/Flask — `@app.get("/path")` перед def
        let mut route_by_line: std::collections::HashMap<usize, (String, String)> =
            std::collections::HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.py_route_re.captures(line) {
                if let (Some(m), Some(p)) = (cap.get(1), cap.get(2)) {
                    route_by_line.insert(i, (m.as_str().to_uppercase(), p.as_str().to_string()));
                }
            }
        }
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.py_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                    let route = [i.checked_sub(2), i.checked_sub(1), Some(i)]
                        .into_iter()
                        .flatten()
                        .find_map(|k| route_by_line.get(&k).cloned());
                    if let Some((method, path)) = route {
                        out.edges.push(AstEdge {
                            source_node_id: node_id,
                            target_node_id: format!("route:{} {}", method, path),
                            label: "ROUTE".to_string(),
                            weight: 1.0,
                            context: format!("{} {}", method, path),
                            provenance: "INFERRED".to_string(),
                        });
                    }
                }
            }
        }
    }

    fn parse_ts_js(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Imports
        for cap in self.ts_import_re.captures_iter(content) {
            if let Some(mod_src) = cap.get(4) {
                let mod_name = mod_src.as_str();
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", mod_name),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("from '{}'", mod_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Classes & Interfaces
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.ts_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("TypeScript/JS класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            } else if let Some(cap) = self.ts_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("Интерфейс/тип `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Functions
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.ts_fn_re.captures(line) {
                let fn_name = cap
                    .get(1)
                    .or_else(|| cap.get(2))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                if !fn_name.is_empty() {
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Функция `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_go(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Structs
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.go_struct_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let struct_name = name_match.as_str();
                    let node_id = format!("struct:{}:{}", rel_path, struct_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: struct_name.to_string(),
                        node_type: "Struct".to_string(),
                        description: format!(
                            "Go структура/интерфейс `{}` в {}",
                            struct_name, rel_path
                        ),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 2. Functions
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.go_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Go функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_sql(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let mut tables = Vec::new();

        for cap in self.sql_table_re.captures_iter(content) {
            if let Some(table_match) = cap.get(1) {
                let table_name = table_match
                    .as_str()
                    .trim_matches(|c| c == '`' || c == '[' || c == ']');
                let node_id = format!("table:{}", table_name);
                tables.push(node_id.clone());
                out.nodes.push(AstNode {
                    node_id: node_id.clone(),
                    label: table_name.to_string(),
                    node_type: "Table".to_string(),
                    description: format!("SQL таблица `{}` в {}", table_name, rel_path),
                    file_path: rel_path.to_string(),
                    line_start: 1,
                    line_end: 1,
                });
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: node_id,
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: format!("CREATE TABLE {}", table_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // Foreign keys
        for cap in self.sql_fk_re.captures_iter(content) {
            if let Some(fk_match) = cap.get(1) {
                let fk_target = fk_match
                    .as_str()
                    .trim_matches(|c| c == '`' || c == '[' || c == ']');
                if let Some(src_table) = tables.first() {
                    out.edges.push(AstEdge {
                        source_node_id: src_table.clone(),
                        target_node_id: format!("table:{}", fk_target),
                        label: "FOREIGN_KEY_TO".to_string(),
                        weight: 1.0,
                        context: format!("REFERENCES {}", fk_target),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_php(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Imports (use)
        for cap in self.php_use_re.captures_iter(content) {
            if let Some(target) = cap.get(1) {
                let target_str = target.as_str().trim();
                if !target_str.is_empty() {
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: format!("module:{}", target_str),
                        label: "IMPORTS".to_string(),
                        weight: 1.0,
                        context: format!("use {};", target_str),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 2. Classes, Interfaces, Traits
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.php_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("PHP класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Extends
                    if let Some(ext_match) = cap.get(2) {
                        let base = ext_match.as_str().trim();
                        if !base.is_empty() {
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!("extends {}", base),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }

                    // Implements
                    if let Some(impl_match) = cap.get(3) {
                        for iface in impl_match.as_str().split(',') {
                            let iface_clean = iface.trim();
                            if !iface_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", iface_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("implements {}", iface_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.php_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("PHP интерфейс `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(ext_match) = cap.get(2) {
                        for base in ext_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if !base_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", base_clean),
                                    label: "INHERITS".to_string(),
                                    weight: 1.0,
                                    context: format!("extends {}", base_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.php_trait_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let trait_name = name_match.as_str();
                    let node_id = format!("trait:{}:{}", rel_path, trait_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: trait_name.to_string(),
                        node_type: "Trait".to_string(),
                        description: format!("PHP трейт `{}` в {}", trait_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Functions & Methods
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.php_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("PHP функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_dart(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Imports
        for cap in self.dart_import_re.captures_iter(content) {
            if let Some(mod_src) = cap.get(1) {
                let mod_name = mod_src.as_str();
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", mod_name),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import '{}';", mod_name),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Classes & Mixins
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.dart_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Dart класс `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Extends
                    if let Some(ext_match) = cap.get(2) {
                        let base = ext_match.as_str().trim();
                        if !base.is_empty() {
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!("extends {}", base),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }

                    // With mixins
                    if let Some(with_match) = cap.get(3) {
                        for mixin in with_match.as_str().split(',') {
                            let mixin_clean = mixin.trim();
                            if !mixin_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("trait:{}", mixin_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("with {}", mixin_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }

                    // Implements
                    if let Some(impl_match) = cap.get(4) {
                        for iface in impl_match.as_str().split(',') {
                            let iface_clean = iface.trim();
                            if !iface_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", iface_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("implements {}", iface_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.dart_mixin_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let mixin_name = name_match.as_str();
                    let node_id = format!("trait:{}:{}", rel_path, mixin_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: mixin_name.to_string(),
                        node_type: "Trait".to_string(),
                        description: format!("Dart миксин `{}` в {}", mixin_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }

        // 3. Functions & Methods
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.dart_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(1) {
                    let fn_name = fn_match.as_str();
                    if matches!(fn_name, "if" | "for" | "while" | "switch" | "catch") {
                        continue;
                    }
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Dart функция/метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }

    fn parse_java(
        &self,
        rel_path: &str,
        file_node_id: &str,
        content: &str,
        out: &mut AstScanResult,
    ) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Package & Imports
        for cap in self.java_package_re.captures_iter(content) {
            if let Some(pkg) = cap.get(1) {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", pkg.as_str()),
                    label: "DEFINES".to_string(),
                    weight: 1.0,
                    context: format!("package {};", pkg.as_str()),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        for cap in self.java_import_re.captures_iter(content) {
            if let Some(imp) = cap.get(1) {
                let imp_str = imp.as_str();
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", imp_str),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {};", imp_str),
                    provenance: "EXTRACTED".to_string(),
                });
            }
        }

        // 2. Classes, Interfaces, Enums, Records
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.java_class_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let class_name = name_match.as_str();
                    let node_id = format!("class:{}:{}", rel_path, class_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: class_name.to_string(),
                        node_type: "Class".to_string(),
                        description: format!("Java класс/тип `{}` в {}", class_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    // Extends
                    if let Some(ext_match) = cap.get(2) {
                        let base = ext_match.as_str().trim();
                        if !base.is_empty() {
                            out.edges.push(AstEdge {
                                source_node_id: node_id.clone(),
                                target_node_id: format!("class:{}", base),
                                label: "INHERITS".to_string(),
                                weight: 1.0,
                                context: format!("extends {}", base),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }

                    // Implements
                    if let Some(impl_match) = cap.get(3) {
                        for iface in impl_match.as_str().split(',') {
                            let iface_clean = iface.trim();
                            if !iface_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", iface_clean),
                                    label: "IMPLEMENTS".to_string(),
                                    weight: 1.0,
                                    context: format!("implements {}", iface_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            } else if let Some(cap) = self.java_interface_re.captures(line) {
                if let Some(name_match) = cap.get(1) {
                    let iface_name = name_match.as_str();
                    let node_id = format!("interface:{}:{}", rel_path, iface_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: iface_name.to_string(),
                        node_type: "Interface".to_string(),
                        description: format!("Java интерфейс `{}` в {}", iface_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id.clone(),
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });

                    if let Some(ext_match) = cap.get(2) {
                        for base in ext_match.as_str().split(',') {
                            let base_clean = base.trim();
                            if !base_clean.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("interface:{}", base_clean),
                                    label: "INHERITS".to_string(),
                                    weight: 1.0,
                                    context: format!("extends {}", base_clean),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 3. Methods
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.java_fn_re.captures(line) {
                if let Some(fn_match) = cap.get(2) {
                    let fn_name = fn_match.as_str();
                    if matches!(fn_name, "if" | "for" | "while" | "switch" | "catch") {
                        continue;
                    }
                    let node_id = format!("fn:{}:{}", rel_path, fn_name);
                    out.nodes.push(AstNode {
                        node_id: node_id.clone(),
                        label: fn_name.to_string(),
                        node_type: "Function".to_string(),
                        description: format!("Java метод `{}` в {}", fn_name, rel_path),
                        file_path: rel_path.to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                    });
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                        provenance: "EXTRACTED".to_string(),
                    });
                }
            }
        }
    }
}
/// Ф39.1: разобрать use-строку Rust → (module_path, item, alias).
/// `crate::a::b::Name as Alias` → ("crate::a::b", Some("Name"), Some("Alias")).
fn split_rust_use(s: &str) -> Option<(String, Option<String>, Option<String>)> {
    let s = s.trim();
    if s.is_empty() || s.contains('{') || s.contains('*') {
        return None; // group/glob imports не резолвим lite-проходом
    }
    let (path, alias) = match s.split_once(" as ") {
        Some((p, a)) => (p.trim(), Some(a.trim().to_string())),
        None => (s, None),
    };
    let parts: Vec<&str> = path.split("::").filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    if parts.len() == 1 {
        return Some((parts[0].to_string(), None, alias));
    }
    let item = parts.last().unwrap().to_string();
    let module = parts[..parts.len() - 1].join("::");
    Some((module, Some(item), alias))
}

/// Ф39.1: кандидаты-файлы, которые может обозначать модуль импорта.
/// Rust: `crate::a::b` → src/a.rs, src/a/b.rs, src/a/b/mod.rs, a.rs…;
/// `super::x`/`self::x` — относительно файла.
/// Python: `.mod`/`pkg.mod` → mod.py, mod/__init__.py.
fn module_to_paths(module: &str, from_file: &str) -> Vec<String> {
    fn parent(p: &str) -> String {
        std::path::Path::new(p)
            .parent()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    let mut out = Vec::new();
    let m = module.trim();
    if m.is_empty() {
        return out;
    }
    if m.contains("::") {
        // Rust
        let ups = m.matches("super::").count();
        let rel = m.trim_start_matches("crate::");
        let rel = rel.trim_start_matches("self::");
        let mut rel = rel.to_string();
        for _ in 0..ups {
            rel = rel.trim_start_matches("super::").to_string();
        }
        let parts: Vec<&str> = rel.split("::").filter(|p| !p.is_empty()).collect();
        let prefix = if m.starts_with("crate::") {
            "src/".to_string()
        } else if ups > 0 {
            let mut d = parent(from_file);
            for _ in 0..ups {
                d = parent(&d);
            }
            if d.is_empty() {
                d
            } else {
                format!("{d}/")
            }
        } else {
            String::new()
        };
        for k in (1..=parts.len()).rev() {
            let b = parts[..k].join("/");
            out.push(format!("{prefix}{b}.rs"));
            out.push(format!("{prefix}{b}/mod.rs"));
        }
        return out;
    }
    if m.starts_with('.') || m.contains('.') {
        // Python
        let dots = m.chars().take_while(|&c| c == '.').count();
        let rest = &m[dots..];
        let mut base = parent(from_file);
        for _ in 1..dots {
            base = parent(&base);
        }
        let prefix = if base.is_empty() {
            String::new()
        } else {
            format!("{base}/")
        };
        if rest.is_empty() {
            out.push(format!("{prefix}__init__.py"));
        } else {
            let rel = rest.replace('.', "/");
            out.push(format!("{prefix}{rel}.py"));
            out.push(format!("{prefix}{rel}/__init__.py"));
        }
    }
    out
}

/// Ф39.1: собрать места вызовов `name(` — без `.`-методов (receiver-типы
/// неизвестны) и макросов `name!(`; ключевые слова скипаются.
fn collect_call_sites(
    rel_path: &str,
    lines: &[&str],
    skip: &[&str],
    re: &Regex,
    out: &mut AstScanResult,
) {
    // строки-определения: `fn helper(` / `def load(` — это не вызовы
    const DEF_STARTERS: &[&str] = &[
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "def ",
        "async def ",
        "class ",
        "trait ",
        "struct ",
        "enum ",
        "impl ",
        "mod ",
        "type ",
    ];
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if DEF_STARTERS.iter().any(|d| trimmed.starts_with(d)) {
            continue;
        }
        for cap in re.captures_iter(line) {
            let Some(m) = cap.get(1) else { continue };
            let name = m.as_str();
            if skip.contains(&name) {
                continue;
            }
            let start = m.start();
            let bytes = line.as_bytes();
            if start > 0 && (bytes[start - 1] == b'.' || bytes[start - 1] == b'!') {
                continue; // AMBIGUOUS: метод с неизвестным receiver / макрос
            }
            out.call_sites.push(CallSite {
                rel_path: rel_path.to_string(),
                line: i + 1,
                name: name.to_string(),
            });
        }
    }
}

/// Ф39.1: лёгкий semantic pass (Rust + Python) — резолв простых вызовов:
/// same-file и по импортам (алиасы учитываются, receiver-типы — нет).
/// Нерезолвленное остаётся без ребра (AMBIGUOUS — не выдумываем).
/// Возвращает число добавленных CALLS-рёбер (provenance = RESOLVED).
pub fn resolve_calls(scan: &mut AstScanResult) -> usize {
    const SYMBOL_TYPES: &[&str] = &[
        "Function",
        "Method",
        "Struct",
        "Class",
        "Trait",
        "Interface",
    ];

    // 1. Таблица символов: label → индексы узлов
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in scan.nodes.iter().enumerate() {
        if SYMBOL_TYPES.contains(&n.node_type.as_str()) {
            by_name.entry(n.label.clone()).or_default().push(i);
        }
    }

    // 2. Импорты и диапазоны символов по файлам
    let mut imports_by_file: HashMap<&str, Vec<&ImportRef>> = HashMap::new();
    for imp in &scan.imports {
        imports_by_file
            .entry(imp.rel_path.as_str())
            .or_default()
            .push(imp);
    }
    let mut syms_by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, n) in scan.nodes.iter().enumerate() {
        if SYMBOL_TYPES.contains(&n.node_type.as_str()) {
            syms_by_file
                .entry(n.file_path.as_str())
                .or_default()
                .push(i);
        }
    }
    for idxs in syms_by_file.values_mut() {
        idxs.sort_by_key(|&i| scan.nodes[i].line_start);
    }

    // 3. Резолв каждого места вызова
    let mut added = 0usize;
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for site in &scan.call_sites {
        // вызывающий — последний символ файла, чей line_start <= строке вызова
        let Some(ci) = syms_by_file.get(site.rel_path.as_str()).and_then(|idxs| {
            let mut caller = None;
            for &i in idxs {
                if scan.nodes[i].line_start <= site.line {
                    caller = Some(i);
                } else {
                    break;
                }
            }
            caller
        }) else {
            continue;
        };

        // алиас → реальное имя элемента; запоминаем модуль алиаса:
        // вызов через алиас обязан уйти в файл этого модуля, не в same-file
        let mut name = site.name.clone();
        let mut alias_module: Option<String> = None;
        if let Some(imps) = imports_by_file.get(site.rel_path.as_str()) {
            for imp in imps {
                if imp.alias.as_deref() == Some(site.name.as_str()) {
                    if let Some(item) = &imp.item {
                        name = item.clone();
                        alias_module = Some(imp.module.clone());
                    }
                }
            }
        }
        let Some(cands) = by_name.get(&name) else {
            continue;
        };

        let import_paths: Vec<String> = imports_by_file
            .get(site.rel_path.as_str())
            .map(|v| {
                v.iter()
                    .flat_map(|imp| module_to_paths(&imp.module, &imp.rel_path))
                    .collect()
            })
            .unwrap_or_default();

        let mut target: Option<(usize, String)> = None;
        if let Some(am) = &alias_module {
            let paths = module_to_paths(am, site.rel_path.as_str());
            for &ti in cands {
                let tn = &scan.nodes[ti];
                if tn.node_id != scan.nodes[ci].node_id && paths.iter().any(|p| p == &tn.file_path)
                {
                    target = Some((ti, format!("typepass:import-alias:{}", tn.file_path)));
                    break;
                }
            }
        }
        if target.is_none() {
            for &ti in cands {
                let tn = &scan.nodes[ti];
                if tn.node_id == scan.nodes[ci].node_id {
                    continue; // рекурсия не даёт ребра в самого себя
                }
                if tn.file_path == site.rel_path {
                    target = Some((ti, "typepass:same-file".to_string()));
                    break;
                }
                if import_paths.iter().any(|p| p == &tn.file_path) {
                    target = Some((ti, format!("typepass:import:{}", tn.file_path)));
                    break;
                }
            }
        }

        if let Some((ti, ctx)) = target {
            let src = scan.nodes[ci].node_id.clone();
            let dst = scan.nodes[ti].node_id.clone();
            if seen.insert((src.clone(), dst.clone())) {
                scan.edges.push(AstEdge {
                    source_node_id: src,
                    target_node_id: dst,
                    label: "CALLS".to_string(),
                    weight: 1.0,
                    context: ctx,
                    provenance: "RESOLVED".to_string(),
                });
                added += 1;
            }
        }
    }
    added
}
