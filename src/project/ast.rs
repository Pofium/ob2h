//! Высокопроизводительный статический AST/код-парсер репозиториев (Rust, Python, TS/JS, Go, SQL, C/C++).
//! Работает полностью детерминированно и локально, без вызовов LLM и без расхода токенов.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};
use regex::Regex;

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
    pub label: String,     // 'IMPORTS' | 'CALLS' | 'IMPLEMENTS' | 'DEFINES' | 'DEPENDS_ON' | 'FOREIGN_KEY_TO'
    pub weight: f64,
    pub context: String,
}

/// Результат AST-сканирования кодовой базы.
#[derive(Debug, Clone, Default)]
pub struct AstScanResult {
    pub files_scanned: usize,
    pub lines_total: usize,
    pub nodes: Vec<AstNode>,
    pub edges: Vec<AstEdge>,
    pub file_hashes: HashMap<String, String>,
}

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
    pub fn scan_directory(&self, root: &Path, known_hashes: Option<&HashMap<String, String>>) -> AstScanResult {
        let mut result = AstScanResult::default();
        let mut files_to_scan = Vec::new();

        self.collect_files(root, root, &mut files_to_scan);

        for (rel_path, abs_path) in files_to_scan {
            if let Ok(bytes) = fs::read(&abs_path) {
                let hash = Self::file_sha256(&bytes);
                result.file_hashes.insert(rel_path.clone(), hash.clone());

                if let Some(known) = known_hashes {
                    if let Some(prev_hash) = known.get(&rel_path) {
                        if prev_hash == &hash {
                            continue; // Файл не изменился
                        }
                    }
                }

                if let Ok(content) = String::from_utf8(bytes) {
                    result.files_scanned += 1;
                    result.lines_total += content.lines().count();
                    self.parse_file(&rel_path, &content, &mut result);
                }
            }
        }

        result
    }

    fn collect_files(&self, root: &Path, current: &Path, out: &mut Vec<(String, PathBuf)>) {
        let entries = match fs::read_dir(current) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Пропускаем служебные и игнорируемые каталоги
            if name.starts_with('.') || name == "target" || name == "node_modules" 
                || name == "dist" || name == "build" || name == "venv" 
                || name == "__pycache__" || name == "vendor" || name == "data" {
                continue;
            }

            if path.is_dir() {
                self.collect_files(root, &path, out);
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if matches!(ext_lower.as_str(), 
                        "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "sql" | "c" | "cpp" | "h" | "hpp" | "php" | "dart" | "java"
                    ) {
                        if let Ok(rel) = path.strip_prefix(root) {
                            let rel_str = rel.to_string_lossy().replace('\\', "/");
                            out.push((rel_str, path));
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

        let ext = Path::new(rel_path).extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();

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

    fn parse_rust(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
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
                });
            }
        }

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
                        description: format!("Структура/перечисление `{}` в {}", struct_name, rel_path),
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
                    });
                }
            }
        }

        // 4. Извлечение functions
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
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: sig.to_string(),
                    });
                }
            }
        }
    }

    fn parse_python(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
        let lines: Vec<&str> = content.lines().collect();

        // 1. Извлечение imports
        for cap in self.py_import_re.captures_iter(content) {
            let mod_name = cap.get(1).or_else(|| cap.get(3)).map(|m| m.as_str().trim()).unwrap_or("");
            if !mod_name.is_empty() {
                out.edges.push(AstEdge {
                    source_node_id: file_node_id.to_string(),
                    target_node_id: format!("module:{}", mod_name),
                    label: "IMPORTS".to_string(),
                    weight: 1.0,
                    context: format!("import {}", mod_name),
                });
            }
        }

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
                                });
                            }
                        }
                    }
                }
            }
        }

        // 3. Извлечение functions
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
                        target_node_id: node_id,
                        label: "DEFINES".to_string(),
                        weight: 1.0,
                        context: line.trim().to_string(),
                    });
                }
            }
        }
    }

    fn parse_ts_js(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
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
                    });
                }
            }
        }

        // 3. Functions
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = self.ts_fn_re.captures(line) {
                let fn_name = cap.get(1).or_else(|| cap.get(2)).map(|m| m.as_str()).unwrap_or("");
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
                        description: format!("Go структура/интерфейс `{}` в {}", struct_name, rel_path),
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
                    });
                }
            }
        }
    }

    fn parse_sql(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
        let mut tables = Vec::new();

        for cap in self.sql_table_re.captures_iter(content) {
            if let Some(table_match) = cap.get(1) {
                let table_name = table_match.as_str().trim_matches(|c| c == '`' || c == '[' || c == ']');
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
                });
            }
        }

        // Foreign keys
        for cap in self.sql_fk_re.captures_iter(content) {
            if let Some(fk_match) = cap.get(1) {
                let fk_target = fk_match.as_str().trim_matches(|c| c == '`' || c == '[' || c == ']');
                if let Some(src_table) = tables.first() {
                    out.edges.push(AstEdge {
                        source_node_id: src_table.clone(),
                        target_node_id: format!("table:{}", fk_target),
                        label: "FOREIGN_KEY_TO".to_string(),
                        weight: 1.0,
                        context: format!("REFERENCES {}", fk_target),
                    });
                }
            }
        }
    }

    fn parse_php(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
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
                    });
                }
            }
        }
    }

    fn parse_dart(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
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
                    });
                }
            }
        }
    }

    fn parse_java(&self, rel_path: &str, file_node_id: &str, content: &str, out: &mut AstScanResult) {
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
                    });
                }
            }
        }
    }
}
