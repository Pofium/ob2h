//! Ф70.2 (PLAN_v2.0): унифицированный интерфейс бэкендов разбора AST.
//! Regex-бэкенд (`AstCodeExtractor` v1.x) и tree-sitter бэкенды (Ф70.3+)
//! скрываются за [`AstParserBackend`]; [`AstParserDispatcher`] выбирает бэкенд
//! прозрачно для вызывающих — скан-флоу не знает о бэкендах.
//!
//! Переключение: фича `treesitter` + поддерживаемый язык → tree-sitter;
//! kill-switch `OB2H_TREESITTER=0` возвращает regex для всех файлов без
//! пересборки (аварийный люк 70.9). Ошибка разбора tree-sitter (в т.ч. доля
//! ERROR-узлов > порога) — автоматический fallback на regex.

use std::sync::Arc;

use crate::project::ast::{AstCodeExtractor, AstScanResult};

/// Унифицированный интерфейс бэкенда разбора одного файла.
pub trait AstParserBackend: Send + Sync {
    /// Разобрать файл и дополнить `out` узлами/рёбрами/сайтами вызовов.
    fn parse_source(&self, rel_path: &str, content: &str, out: &mut AstScanResult);
    /// Имя бэкенда для телеметрии: `regex` | `treesitter:<lang>`.
    fn backend_name(&self) -> &str;
}

/// Regex-бэкенд — текущий экстрактор v1.x (AST_EXTRACTOR_VERSION = 45).
pub struct RegexBackend {
    extractor: Arc<AstCodeExtractor>,
}

impl RegexBackend {
    pub fn new() -> Self {
        Self {
            extractor: Arc::new(AstCodeExtractor::new()),
        }
    }

    /// Доступ к экстрактору (сбор файлов, скан через колбэк 70.2).
    pub fn extractor(&self) -> &AstCodeExtractor {
        &self.extractor
    }
}

impl Default for RegexBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AstParserBackend for RegexBackend {
    fn parse_source(&self, rel_path: &str, content: &str, out: &mut AstScanResult) {
        self.extractor.parse_file(rel_path, content, out);
    }

    fn backend_name(&self) -> &str {
        "regex"
    }
}

/// Языки с реализованным CST→AST-маппингом в `AstScanResult`:
/// Ф70.3 — rust; Ф70.4/70.5/70.6/70.12 добавят ts/python/go/dart.
#[cfg(feature = "treesitter")]
pub fn has_mapper(lang: &str) -> bool {
    matches!(lang, "rust")
}

/// Язык tree-sitter для пути по расширению: rust | python | ts | go | dart.
#[cfg(feature = "treesitter")]
pub fn lang_for_path(rel_path: &str) -> Option<&'static str> {
    let ext = rel_path.rsplit('.').next()?;
    Some(match ext {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" | "js" | "jsx" => "ts",
        "go" => "go",
        "dart" => "dart",
        _ => return None,
    })
}

/// Tree-sitter движок (Ф70.3+): разбор CST и маппинг в `AstScanResult`.
/// Без фичи `treesitter` тип не существует, диспетчер всегда уходит в regex.
#[cfg(feature = "treesitter")]
pub mod ts {
    use std::collections::HashMap;

    use tree_sitter::{Node, Parser};

    use crate::project::ast::{
        collect_call_sites_standalone, split_rust_use, AstEdge, AstNode, AstScanResult,
    };

    /// Грамматика дерева по имени языка.
    fn language_for(lang: &str) -> Option<tree_sitter::Language> {
        Some(match lang {
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "python" => tree_sitter_python::LANGUAGE.into(),
            "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "go" => tree_sitter_go::LANGUAGE.into(),
            "dart" => tree_sitter_dart::LANGUAGE.into(),
            _ => return None,
        })
    }

    /// Схлопывание пробелов (формат сигнатур regex-бэкенда).
    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// `#[get("/path")]` → (метод, путь); None — не маршрутный атрибут.
    fn parse_route_attr(inner: &str) -> Option<(String, String)> {
        let open = inner.find('(')?;
        let method = inner[..open].trim();
        if !matches!(
            method.to_ascii_lowercase().as_str(),
            "get" | "post" | "put" | "delete" | "patch" | "head" | "options" | "route"
        ) {
            return None;
        }
        let rest = &inner[open..];
        let start = rest.find('"')? + 1;
        let end = rest[start..].find('"')? + start;
        Some((method.to_ascii_uppercase(), rest[start..end].to_string()))
    }

    /// Разобрать файл tree-sitter'ом и смаппить в `AstScanResult`.
    /// Маппинг по языкам — Ф70.3 (rust), Ф70.4 (ts/js), Ф70.5 (python),
    /// Ф70.6 (go), Ф70.12 (dart); до реализации соответствующего маппера
    /// возвращается Err — диспетчер уходит в regex (70.9).
    pub fn parse_source(
        rel_path: &str,
        content: &str,
        lang: &str,
        out: &mut AstScanResult,
    ) -> anyhow::Result<()> {
        let grammar =
            language_for(lang).ok_or_else(|| anyhow::anyhow!("нет грамматики `{lang}`"))?;
        let mut parser = Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| anyhow::anyhow!("set_language `{lang}`: {e}"))?;
        let Some(tree) = parser.parse(content, None) else {
            anyhow::bail!("parser.parse вернул None для {rel_path}");
        };
        let root = tree.root_node();
        // 70.9: регламент fallback — ERROR-узлы в CST уводят файл в regex.
        // Долевой порог (>10% ERROR) — точная политика 70.9; пока строгий вариант.
        if root.has_error() {
            anyhow::bail!("CST `{rel_path}` содержит ERROR-узлы — fallback на regex");
        }
        match lang {
            "rust" => {
                map_rust(rel_path, content, root, out);
                Ok(())
            }
            _ => Err(anyhow::anyhow!(
                "маппинг `{lang}` ещё не реализован (Ф70.4+)"
            )),
        }
    }

    /// Ф70.3 (PLAN_v2.0): CST→AST маппинг Rust (tree-sitter-rust).
    /// node_id и описания зеркалят regex-бэкенд — стабильность привязки памяти
    /// и findings Ralph к node_id; provenance — словарь v1.x (EXTRACTED/INFERRED),
    /// переход на SYNTAX/HEURISTIC — единый в 71.3 (M12). tree-sitter даёт
    /// superset: методы `impl`-блоков и сигнатуры трейтов (function_signature_item),
    /// которые regex-бэкенд терял.
    pub fn map_rust(rel_path: &str, content: &str, root: Node, out: &mut AstScanResult) {
        let file_node_id = format!("file:{rel_path}");
        out.nodes.push(AstNode {
            node_id: file_node_id.clone(),
            label: rel_path.to_string(),
            node_type: "File".to_string(),
            description: format!("Исходный файл проекта: {rel_path}"),
            file_path: rel_path.to_string(),
            line_start: 1,
            line_end: content.lines().count().max(1),
        });

        // 1. Маршруты axum/actix (Ф40.2): #[get("/path")] → (МЕТОД, путь) по строке.
        let src = content.as_bytes();
        let mut route_by_line: HashMap<usize, (String, String)> = HashMap::new();
        collect_routes(root, src, &mut route_by_line);

        // 2. Рекурсивный обход CST: символы верхнего уровня, impl-методы,
        //    сигнатуры трейтов, use-импорты.
        walk_rust(root, src, rel_path, &file_node_id, &route_by_line, out);

        // 3. Места вызовов для type-pass (тот же контракт, что у regex-пути).
        collect_call_sites_standalone(rel_path, content, out);
    }

    /// Собрать карту «строка → (метод, путь)» из маршрутных атрибутов.
    fn collect_routes(root: Node, src: &[u8], out: &mut HashMap<usize, (String, String)>) {
        let mut cursor = root.walk();
        visit_routes(root, &mut cursor, src, out);
    }

    fn visit_routes(
        node: Node,
        cursor: &mut tree_sitter::TreeCursor,
        src: &[u8],
        out: &mut HashMap<usize, (String, String)>,
    ) {
        if node.kind() == "attribute_item" {
            if let Ok(text) = node.utf8_text(src) {
                let inner = text
                    .trim()
                    .trim_start_matches('#')
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim();
                if let Some((method, path)) = parse_route_attr(inner) {
                    out.insert(node.start_position().row, (method, path));
                }
            }
        }
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                visit_routes(child, cursor, src, out);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }

    /// Рекурсивный обход CST: struct/enum/trait/function (+impl-методы,
    /// function_signature_item в трейтах), use-импорты; node_id/описания —
    /// точное зеркало regex-бэкенда.
    fn walk_rust(
        node: Node,
        src: &[u8],
        rel_path: &str,
        file_node_id: &str,
        route_by_line: &HashMap<usize, (String, String)>,
        out: &mut AstScanResult,
    ) {
        let text_of = |n: Node| n.utf8_text(src).unwrap_or("").to_string();
        match node.kind() {
            "use_declaration" => {
                let raw = text_of(node);
                let inner = raw
                    .trim()
                    .strip_prefix("use")
                    .unwrap_or(raw.trim())
                    .trim()
                    .trim_end_matches(';')
                    .trim();
                if !inner.is_empty() {
                    out.edges.push(AstEdge {
                        source_node_id: file_node_id.to_string(),
                        target_node_id: format!("module:{inner}"),
                        label: "IMPORTS".to_string(),
                        weight: 1.0,
                        context: format!("use {inner};"),
                        provenance: "EXTRACTED".to_string(),
                    });
                    if let Some((module, item, alias)) = split_rust_use(inner) {
                        out.imports.push(crate::project::ast::ImportRef {
                            rel_path: rel_path.to_string(),
                            module,
                            item,
                            alias,
                        });
                    }
                }
            }
            "struct_item" | "enum_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text_of(name_node);
                    if !name.is_empty() {
                        let node_id = format!("struct:{rel_path}:{name}");
                        let line = node.start_position().row + 1;
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: name.clone(),
                            node_type: "Struct".to_string(),
                            description: format!("Структура/перечисление `{name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id,
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: node
                                .utf8_text(src)
                                .unwrap_or("")
                                .lines()
                                .next()
                                .unwrap_or("")
                                .trim()
                                .to_string(),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
            "trait_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text_of(name_node);
                    if !name.is_empty() {
                        let node_id = format!("trait:{rel_path}:{name}");
                        let line = node.start_position().row + 1;
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: name.clone(),
                            node_type: "Trait".to_string(),
                            description: format!("Трейт `{name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id,
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: node
                                .utf8_text(src)
                                .unwrap_or("")
                                .lines()
                                .next()
                                .unwrap_or("")
                                .trim()
                                .to_string(),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
            "function_item" | "function_signature_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let fn_name = text_of(name_node);
                    if !fn_name.is_empty() {
                        let node_id = format!("fn:{rel_path}:{fn_name}");
                        // Сигнатура: от начала узла до начала тела (или весь
                        // узел для сигнатуры трейта), пробелы схлопнуты —
                        // формат описания regex-бэкенда.
                        let sig = match node.child_by_field_name("body") {
                            Some(body) => {
                                let start = node.start_byte();
                                let end = body.start_byte();
                                collapse_ws(&node.utf8_text(src).unwrap_or("")
                                    [..end - start])
                            }
                            None => collapse_ws(&text_of(node)),
                        };
                        let line = name_node.start_position().row + 1;
                        let line_end = node
                            .child_by_field_name("body")
                            .map(|b| b.end_position().row + 1)
                            .unwrap_or(line);
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: fn_name.clone(),
                            node_type: "Function".to_string(),
                            description: format!("Функция `{fn_name}` ({sig}) в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line_end.max(line),
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: sig.clone(),
                            provenance: "EXTRACTED".to_string(),
                        });
                        // Ф40.2: ROUTE — атрибут на строке fn или одной из двух
                        // предыдущих (0-based ключи), provenance=INFERRED.
                        let row0 = name_node.start_position().row;
                        let route = [Some(row0), row0.checked_sub(1), row0.checked_sub(2)]
                            .into_iter()
                            .flatten()
                            .find_map(|k| route_by_line.get(&k).cloned());
                        if let Some((method, path)) = route {
                            out.edges.push(AstEdge {
                                source_node_id: node_id,
                                target_node_id: format!("route:{method} {path}"),
                                label: "ROUTE".to_string(),
                                weight: 1.0,
                                context: format!("{method} {path}"),
                                provenance: "INFERRED".to_string(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
        // Рекурсия (impl/mod bodies и прочие контейнеры).
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                walk_rust(child, src, rel_path, file_node_id, route_by_line, out);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }
}


/// Диспетчер бэкендов: прозрачный выбор regex / tree-sitter на файл.
/// Создаётся один раз на скан (конструирование regex'ов дорогое).
pub struct AstParserDispatcher {
    regex: RegexBackend,
    #[cfg(feature = "treesitter")]
    ts_enabled: bool,
}

/// Tree-sitter включён: фича собрана и env-kill-switch не запрещает.
/// `OB2H_TREESITTER=0` — аварийный возврат на regex без пересборки.
#[cfg_attr(not(feature = "treesitter"), allow(dead_code))]
fn ts_enabled_from_env(v: Option<String>) -> bool {
    match v {
        Some(val) => val != "0",
        None => true,
    }
}

impl AstParserDispatcher {
    pub fn new() -> Self {
        Self {
            regex: RegexBackend::new(),
            #[cfg(feature = "treesitter")]
            ts_enabled: Self::ts_enabled(),
        }
    }

    /// Tree-sitter включён: kill-switch `OB2H_TREESITTER=0` не запрещает.
    #[cfg(feature = "treesitter")]
    fn ts_enabled() -> bool {
        ts_enabled_from_env(std::env::var("OB2H_TREESITTER").ok())
    }

    /// Прозрачный разбор файла: tree-sitter при поддержке языка (фича собрана,
    /// kill-switch не запрещает, маппер реализован), иначе regex (70.9).
    /// Возвращает имя использованного бэкенда — телеметрия.
    pub fn parse_source(&self, rel_path: &str, content: &str, out: &mut AstScanResult) -> String {
        #[cfg(feature = "treesitter")]
        if self.ts_enabled {
            if let Some(lang) = lang_for_path(rel_path) {
                if has_mapper(lang) {
                    // 70.9: ошибка tree-sitter (в т.ч. ERROR-узлы) — fallback на regex
                    if let Ok(()) = ts::parse_source(rel_path, content, lang, out) {
                        return format!("treesitter:{lang}");
                    }
                }
            }
        }
        self.regex.parse_source(rel_path, content, out);
        "regex".to_string()
    }

    /// Скан каталога через диспетчер (тот же контракт, что у экстрактора).
    pub fn scan_directory(
        &self,
        root: &std::path::Path,
        known_hashes: Option<&std::collections::HashMap<String, String>>,
    ) -> AstScanResult {
        self.regex
            .extractor()
            .scan_directory_with(root, known_hashes, &mut |rel_path, content, out| {
                self.parse_source(rel_path, content, out)
            })
    }
}

impl Default for AstParserDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_env_kill_switch() {
        assert!(ts_enabled_from_env(None));
        assert!(ts_enabled_from_env(Some("1".into())));
        assert!(ts_enabled_from_env(Some("".into())));
        assert!(!ts_enabled_from_env(Some("0".into())));
    }

    /// Диспетчер даёт те же node_id, что и прямой regex-экстрактор:
    /// без фичи — всегда regex; с фичей — tree-sitter:rust (маппер зеркалит
    /// node_id regex-бэкенда — привязка памяти стабильна при смене бэкенда).
    #[test]
    fn dispatcher_matches_direct_extractor() {
        let src = "pub fn run_pipeline() { let x = build_cache(); }";
        let direct = {
            let ex = AstCodeExtractor::new();
            let mut r = AstScanResult::default();
            ex.parse_file("src/a.rs", src, &mut r);
            r
        };
        let disp = AstParserDispatcher::new();
        let mut r = AstScanResult::default();
        let backend = disp.parse_source("src/a.rs", src, &mut r);
        #[cfg(feature = "treesitter")]
        assert_eq!(backend, "treesitter:rust");
        #[cfg(not(feature = "treesitter"))]
        assert_eq!(backend, "regex");
        let ids: std::collections::HashSet<&str> =
            r.nodes.iter().map(|n| n.node_id.as_str()).collect();
        let ids_direct: std::collections::HashSet<&str> =
            direct.nodes.iter().map(|n| n.node_id.as_str()).collect();
        assert_eq!(ids, ids_direct, "node_id должны совпадать с regex-бэкендом");
    }
}
