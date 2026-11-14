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

#[cfg_attr(not(feature = "treesitter"), allow(unused_imports))]
use crate::project::ast::{AstCodeExtractor, AstEdge, AstNode, AstScanResult, CallSite};

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
    matches!(lang, "rust" | "python" | "go" | "dart" | "ts" | "tsx")
}

/// Язык tree-sitter для пути по расширению: rust | python | ts | go | dart.
#[cfg(feature = "treesitter")]
pub fn lang_for_path(rel_path: &str) -> Option<&'static str> {
    let ext = rel_path.rsplit('.').next()?;
    Some(match ext {
        "rs" => "rust",
        "py" => "python",
        "ts" | "js" => "ts",
        "tsx" | "jsx" => "tsx",
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

    use tree_sitter::{Node, Parser, StreamingIterator, Tree};

    /// Ф70.9 (PLAN_v2.0): порог доли ERROR-узлов в CST, выше которого файл
    /// уходит в regex (tree-sitter на частично битых файлах работает толерантно —
    /// fallback только для сильно сломанных).
    pub const ERROR_FALLBACK_FRACTION: f64 = 0.10;

    /// Доля ERROR-узлов в CST: (error_nodes, total_nodes).
    fn error_fraction(root: Node) -> (usize, usize) {
        fn count(node: Node, errors: &mut usize, total: &mut usize) {
            if node.is_error() {
                *errors += 1;
            }
            *total += 1;
            let mut c = node.walk();
            for child in node.children(&mut c) {
                count(child, errors, total);
            }
        }
        let mut errors = 0usize;
        let mut total = 0usize;
        count(root, &mut errors, &mut total);
        (errors, total)
    }

    use crate::project::ast::{
        collect_call_sites_standalone, split_rust_use, AstEdge, AstNode, AstScanResult, CallSite,
    };

    /// Грамматика дерева по имени языка (pub — кросс-проверки интеграционных тестов).
    pub fn language_for(lang: &str) -> Option<tree_sitter::Language> {
        Some(match lang {
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "python" => tree_sitter_python::LANGUAGE.into(),
            "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            "go" => tree_sitter_go::LANGUAGE.into(),
            "dart" => tree_sitter_dart::LANGUAGE.into(),
            _ => return None,
        })
    }

    /// Схлопывание пробелов (формат сигнатур regex-бэкенда).
    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Общий узел File (зеркало parse_file) — возвращает `file:{rel}`.
    pub fn push_file_node(out: &mut AstScanResult, rel_path: &str, content: &str) -> String {
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
        file_node_id
    }

    /// Прямой ребёнок заданного kind (не рекурсивно).
    fn first_direct_child<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        let mut c = node.walk();
        if !c.goto_first_child() {
            return None;
        }
        loop {
            let child = c.node();
            if child.kind() == kind {
                return Some(child);
            }
            if !c.goto_next_sibling() {
                return None;
            }
        }
    }

    /// DFS по поддереву: первый узел заданного kind.
    fn find_descendant<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut c = node.walk();
        for child in node.children(&mut c) {
            if let Some(found) = find_descendant(child, kind) {
                return Some(found);
            }
        }
        None
    }

    /// Все type_identifier в поддереве (имена типов: базы/миксины/интерфейсы).
    #[allow(dead_code)]
    fn type_identifiers_in(node: Node, src: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        collect_type_ids(node, src, &mut out);
        out
    }

    fn collect_type_ids(node: Node, src: &[u8], out: &mut Vec<String>) {
        if node.kind() == "type_identifier" {
            if let Ok(t) = node.utf8_text(src) {
                out.push(t.to_string());
            }
        }
        let mut c = node.walk();
        for child in node.children(&mut c) {
            collect_type_ids(child, src, out);
        }
    }

    /// Тексты прямых детей kind="type" (базы класса в секции superclass/mixins).
    fn direct_type_texts(node: Node, src: &[u8]) -> Vec<String> {
        let mut c = node.walk();
        node.children(&mut c)
            .filter(|ch| ch.kind() == "type")
            .filter_map(|ch| ch.utf8_text(src).ok().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect()
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
    /// Разобрать файл tree-sitter'ом (cold, без LRU-кэша) и смаппить в
    /// `AstScanResult` — тест-путь; боевой путь диспетчера — через
    /// [`IncrementalParser`] (70.8) с кэш-хитами и Tree::edit.
    pub fn parse_source(
        rel_path: &str,
        content: &str,
        lang: &str,
        out: &mut AstScanResult,
    ) -> anyhow::Result<()> {
        let tree = cold_parse_tree(rel_path, content, lang)?;
        let root = tree.root_node();
        check_error_fraction(rel_path, root)?;
        map_tree(lang, rel_path, content, root, out);
        Ok(())
    }

    /// Cold-парсинг дерева без кэша (тест-путь и основа инкрементального).
    fn cold_parse_tree(rel_path: &str, content: &str, lang: &str) -> anyhow::Result<Tree> {
        let grammar =
            language_for(lang).ok_or_else(|| anyhow::anyhow!("нет грамматики `{lang}`"))?;
        let mut parser = Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| anyhow::anyhow!("set_language `{lang}`: {e}"))?;
        let Some(tree) = parser.parse(content, None) else {
            anyhow::bail!("parser.parse вернул None для {rel_path}");
        };
        Ok(tree)
    }

    /// Ф70.9: доля ERROR-узлов > порога → Err (файл уходит в regex);
    /// меньшая доля допустима — tree-sitter парсит толерантно.
    fn check_error_fraction(rel_path: &str, root: Node) -> anyhow::Result<()> {
        let (err_nodes, total_nodes) = error_fraction(root);
        if total_nodes > 0 && err_nodes as f64 / total_nodes as f64 > ERROR_FALLBACK_FRACTION {
            anyhow::bail!(
                "CST `{rel_path}`: {err_nodes}/{total_nodes} ERROR-узлов (> {ERROR_FALLBACK_FRACTION}) — fallback на regex"
            );
        }
        Ok(())
    }

    /// Маппинг разобранного дерева по языку (общий для cold и инкрементального путей).
    pub fn map_tree(
        lang: &str,
        rel_path: &str,
        content: &str,
        root: Node,
        out: &mut AstScanResult,
    ) {
        match lang {
            "rust" => map_rust(rel_path, content, root, out),
            "python" => map_python(rel_path, content, root, out),
            "go" => map_go(rel_path, content, root, out),
            "dart" => map_dart(rel_path, content, root, out),
            "ts" | "tsx" => map_ts_js(rel_path, content, root, out),
            _ => {} // has_mapper не пропускает прочие языки
        }
    }

    /// Ф70.8 (PLAN_v2.0): инкрементальный парсер — LRU-кэш деревьев последних
    /// изменённых файлов (64 записи; тотальное хранение деревьев противоречит
    /// NFR по RAM). Хит с тем же контентом → clone дерева без парсинга;
    /// изменённый файл → Tree::edit по префикс/суффикс-диффу + reparse со
    /// старым деревом (tree-sitter переиспользует поддеревья).
    pub struct IncrementalParser {
        cache: std::sync::Mutex<lru::LruCache<String, Entry>>,
    }

    struct Entry {
        tree: Tree,
        content: String,
    }

    impl IncrementalParser {
        pub fn new() -> Self {
            Self {
                cache: std::sync::Mutex::new(lru::LruCache::new(
                    std::num::NonZeroUsize::new(64).unwrap(),
                )),
            }
        }

        /// Разобрать файл: хит+тот же контент → дерево из кэша; хит+изменение →
        /// Tree::edit + reparse со старым деревом; промах → cold-парсинг.
        /// ERROR-доля сверх порога (70.9) → Err без кэширования.
        pub fn parse_tree(
            &self,
            rel_path: &str,
            content: &str,
            lang: &str,
        ) -> anyhow::Result<Tree> {
            let mut cache = self.cache.lock().expect("lru cache lock");
            if let Some(entry) = cache.get(rel_path) {
                if entry.content == content {
                    return Ok(entry.tree.clone());
                }
            }
            let grammar =
                language_for(lang).ok_or_else(|| anyhow::anyhow!("нет грамматики `{lang}`"))?;
            let mut parser = Parser::new();
            parser
                .set_language(&grammar)
                .map_err(|e| anyhow::anyhow!("set_language `{lang}`: {e}"))?;
            let cached = cache.get(rel_path).map(|e| (e.content.clone(), e.tree.clone()));
            let tree = if let Some((old_content, mut old_tree)) = cached {
                // Инкрементальный путь: дифф префикс/суффикс → InputEdit
                if let Some(edit) = compute_edit(&old_content, content) {
                    old_tree.edit(&edit);
                }
                parser.parse(content, Some(&old_tree)).ok_or_else(|| {
                    anyhow::anyhow!("parser.parse вернул None для {rel_path}")
                })?
            } else {
                parser.parse(content, None)
                    .ok_or_else(|| anyhow::anyhow!("parser.parse вернул None для {rel_path}"))?
            };
            let root = tree.root_node();
            let (err_nodes, total_nodes) = error_fraction(root);
            if total_nodes > 0 && err_nodes as f64 / total_nodes as f64 > ERROR_FALLBACK_FRACTION {
                anyhow::bail!(
                    "CST `{rel_path}`: {err_nodes}/{total_nodes} ERROR-узлов — fallback на regex"
                );
            }
            cache.put(
                rel_path.to_string(),
                Entry {
                    tree: tree.clone(),
                    content: content.to_string(),
                },
            );
            Ok(tree)
        }
    }

    impl Default for IncrementalParser {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Префикс/суффикс-дифф старого и нового контента → tree_sitter::InputEdit.
    /// None — контент идентичен.
    fn compute_edit(old: &str, new: &str) -> Option<tree_sitter::InputEdit> {
        use tree_sitter::{InputEdit, Point};
        let (ob, nb) = (old.as_bytes(), new.as_bytes());
        if old == new {
            return None;
        }
        let min_len = ob.len().min(nb.len());
        let mut start = 0usize;
        while start < min_len && ob[start] == nb[start] {
            start += 1;
        }
        while start > 0 && !old.is_char_boundary(start) {
            start -= 1;
        }
        // Суффикс: от концов назад, не заходя в общий префикс
        let mut end_old = ob.len();
        let mut end_new = nb.len();
        while end_old > start && end_new > start && ob[end_old - 1] == nb[end_new - 1] {
            end_old -= 1;
            end_new -= 1;
        }
        while !old.is_char_boundary(end_old) || !new.is_char_boundary(end_new) {
            end_old = end_old.saturating_sub(1);
            end_new = end_new.saturating_sub(1);
        }
        let point_at = |s: &str, byte: usize| -> Point {
            let mut row = 0usize;
            let mut col = 0usize;
            for (i, ch) in s.char_indices() {
                if i >= byte {
                    break;
                }
                if ch == '\n' {
                    row += 1;
                    col = 0;
                } else {
                    col += ch.len_utf16();
                }
            }
            Point { row, column: col }
        };
        Some(InputEdit {
            start_byte: start,
            old_end_byte: end_old,
            new_end_byte: end_new,
            start_position: point_at(old, start),
            old_end_position: point_at(old, end_old),
            new_end_position: point_at(new, end_new),
        })
    }

    /// Ф70.3 (PLAN_v2.0): CST→AST маппинг Rust (tree-sitter-rust).
    /// node_id и описания зеркалят regex-бэкенд — стабильность привязки памяти
    /// и findings Ralph к node_id; provenance — словарь v1.x (EXTRACTED/INFERRED),
    /// переход на SYNTAX/HEURISTIC — единый в 71.3 (M12). tree-sitter даёт
    /// superset: методы `impl`-блоков и сигнатуры трейтов (function_signature_item),
    /// которые regex-бэкенд терял.
    pub fn map_rust(rel_path: &str, content: &str, root: Node, out: &mut AstScanResult) {
        let file_node_id = push_file_node(out, rel_path, content);

        // 1. Маршруты axum/actix (Ф40.2): #[get("/path")] → (МЕТОД, путь) по строке.
        let src = content.as_bytes();
        let mut route_by_line: HashMap<usize, (String, String)> = HashMap::new();
        collect_routes(root, src, &mut route_by_line);

        // 2. Рекурсивный обход CST: символы верхнего уровня, impl-методы,
        //    сигнатуры трейтов, use-импорты.
        walk_rust(root, src, rel_path, &file_node_id, &route_by_line, out);

        // 3. Места вызовов для type-pass (тот же контракт, что у regex-пути).
        collect_call_sites_standalone(rel_path, content, crate::project::ast::RUST_CALL_SKIP, out);
        // 71.1: дедуп мест вызовов после CST + line-based проходов
        out.call_sites.sort();
        out.call_sites.dedup();
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
                        // 71.2: тестовая функция — #[test]/#[tokio::test] на
                        // предыдущих siblings (kind_detail='test' при записи)
                        let is_test = node.parent().map(|p| {
                            let mut pc = p.walk();
                            let mut found = false;
                            for sib in p.children(&mut pc) {
                                if sib.id() == node.id() {
                                    break;
                                }
                                if sib.kind() == "attribute_item" {
                                    if let Ok(t) = sib.utf8_text(src) {
                                        let tt = t.trim();
                                        if tt.starts_with("#[test") || tt.starts_with("#[tokio::test") {
                                            found = true;
                                        }
                                    }
                                }
                                if found {
                                    break;
                                }
                            }
                            found
                        }).unwrap_or(false);
                        if is_test {
                            out.test_node_ids.push(node_id.clone());
                        }
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
            "impl_item" => {
                // 71.2: impl Trait for Type → IMPLEMENTS (цель — голое имя трейта,
                // как в dart/ts-эвристиках; source — структурный узел того же
                // файла или файл-узел). Рекурсия продолжит собрать методы.
                let trait_name = node.child_by_field_name("trait_").map(|t| text_of(t));
                if let Some(t) = trait_name {
                    if !t.is_empty() {
                        let type_name = node.child_by_field_name("type_").and_then(|ty| {
                            if ty.kind() == "type_identifier" {
                                Some(text_of(ty))
                            } else {
                                find_descendant(ty, "type_identifier").map(|n| text_of(n))
                            }
                        });
                        if let Some(type_name) = type_name {
                            let source = if out
                                .nodes
                                .iter()
                                .any(|n| n.label == type_name && n.file_path == rel_path)
                            {
                                format!("struct:{rel_path}:{type_name}")
                            } else {
                                format!("file:{rel_path}")
                            };
                            out.edges.push(AstEdge {
                                source_node_id: source,
                                target_node_id: format!("trait:{t}"),
                                label: "IMPLEMENTS".to_string(),
                                weight: 1.0,
                                context: format!("impl {t} for {type_name}"),
                                provenance: "EXTRACTED".to_string(),
                            });
                        }
                    }
                }
            }
            "call_expression" => {
                // 71.1: имя вызова — identifier (прямые) или field_expression.field
                // (obj.method()); цепочки a.b().c() дают каждое звено. Ключевые
                // слова (if/match/…) не являются call_expression — отсекаются
                // структурно, skip-списки regex-пути не нужны.
                // 71.4: qualifier — приёмник вызова (scoped path `helpers::run`,
                // простой идентификатор `store.fetch`) для file-scope резолва.
                if let Some(f) = node.child_by_field_name("function") {
                    let (name, qualifier) = match f.kind() {
                        "identifier" => (text_of(f), None),
                        "field_expression" => {
                            let name = f
                                .child_by_field_name("field")
                                .map(|n| text_of(n))
                                .unwrap_or_default();
                            // tree-sitter-rust: поле объекта — `value`
                            let q = f
                                .child_by_field_name("value")
                                .or_else(|| f.child_by_field_name("object"))
                                .filter(|o| o.kind() == "identifier")
                                .map(|o| text_of(o));
                            (name, q)
                        }
                        "scoped_identifier" => {
                            // `helpers::run()` / `Type::method()` — path целиком
                            let name = f
                                .child_by_field_name("name")
                                .map(|n| text_of(n))
                                .unwrap_or_default();
                            let q = f
                                .child_by_field_name("path")
                                .map(|p| text_of(p))
                                .unwrap_or_default();
                            (name, if q.is_empty() { None } else { Some(q) })
                        }
                        _ => (String::new(), None),
                    };
                    if !name.is_empty() {
                        out.call_sites.push(CallSite {
                            rel_path: rel_path.to_string(),
                            line: node.start_position().row + 1,
                            name,
                            qualifier,
                        });
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

    /// Ф70.5 (PLAN_v2.0): CST→AST маппинг Python (tree-sitter-python).
    /// Зеркало regex-бэкенда: классы (+Pydantic/dataclass пометка, INHERITS),
    /// функции/методы (с декораторами в описании), FastAPI/Flask ROUTE,
    /// `__tablename__` (QUERIES_TABLE — поиск внутри тела класса, точнее
    /// построчной эвристики regex), импорты + ImportRef, call_sites (PY_CALL_SKIP).
    pub fn map_python(rel_path: &str, content: &str, root: Node, out: &mut AstScanResult) {
        let file_node_id = push_file_node(out, rel_path, content);

        let src = content.as_bytes();
        // Маршруты FastAPI/Flask: ключ — последняя строка блока декораторов
        // (зеркало regex-эвристики route_by_line с ключом end).
        let mut route_by_line: HashMap<usize, (String, String)> = HashMap::new();
        collect_py_routes(root, src, &mut route_by_line);

        walk_python(root, src, rel_path, &file_node_id, &route_by_line, out);

        // Места вызовов для type-pass (словарь python).
        collect_call_sites_standalone(rel_path, content, crate::project::ast::PY_CALL_SKIP, out);
        out.call_sites.sort();
        out.call_sites.dedup();
    }

    /// Собрать карту «строка → (метод, путь)» из декораторов `@app.get("/x")`
    /// (регулярка regex-бэкенда: объекты app|router|api, методы get|post|...).
    fn collect_py_routes(root: Node, src: &[u8], out: &mut HashMap<usize, (String, String)>) {
        visit_py_routes(root, src, out);
    }

    fn visit_py_routes(node: Node, src: &[u8], out: &mut HashMap<usize, (String, String)>) {
        if node.kind() == "decorator" {
            if let Ok(text) = node.utf8_text(src) {
                if let Some((method, path)) = parse_py_route_attr(text) {
                    // Ключ — конец блока декоратора (последняя его строка),
                    // как в regex-пути (decorator_block_end).
                    out.insert(node.end_position().row, (method, path));
                }
            }
        }
        let mut c = node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                visit_py_routes(child, src, out);
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }

    /// `@app.get("/path")` → (GET, /path) — зеркало py_route_re
    /// (объекты app|router|api; методы get|post|put|delete|patch).
    fn parse_py_route_attr(text: &str) -> Option<(String, String)> {
        let t = text.trim().trim_start_matches('@');
        let open = t.find('(')?;
        let head = t[..open].trim(); // app.get
        let args = &t[open..]; // ("/path") — путь берём из аргументов
        let (obj, method) = head.split_once('.')?;
        if !matches!(obj, "app" | "router" | "api") {
            return None;
        }
        if !matches!(
            method.to_ascii_lowercase().as_str(),
            "get" | "post" | "put" | "delete" | "patch"
        ) {
            return None;
        }
        let start = args.find('"').or_else(|| args.find('\''))? + 1;
        let end = args[start..].find('"').or_else(|| args[start..].find('\''))? + start;
        Some((method.to_ascii_uppercase(), args[start..end].to_string()))
    }

    /// Декораторы родительского decorated_definition (для описания узла).
    fn decorators_of(node: Node, src: &[u8]) -> Vec<String> {
        let mut decos = Vec::new();
        if let Some(parent) = node.parent() {
            if parent.kind() == "decorated_definition" {
                let mut c = parent.walk();
                for child in parent.children(&mut c) {
                    if child.kind() == "decorator" {
                        if let Ok(t) = child.utf8_text(src) {
                            decos.push(t.trim().to_string());
                        }
                    }
                }
            }
        }
        decos
    }

    /// Рекурсивный обход Python CST.
    fn walk_python(
        node: Node,
        src: &[u8],
        rel_path: &str,
        file_node_id: &str,
        route_by_line: &HashMap<usize, (String, String)>,
        out: &mut AstScanResult,
    ) {
        let text_of = |n: Node| n.utf8_text(src).unwrap_or("").to_string();
        match node.kind() {
            "import_statement" => {
                // import x.y, z as w — dotted_name / aliased_import дети
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    match child.kind() {
                        "dotted_name" => {
                            let module = text_of(child);
                            push_py_import_edge(out, file_node_id, &module);
                            out.imports.push(crate::project::ast::ImportRef {
                                rel_path: rel_path.to_string(),
                                module: module.clone(),
                                item: None,
                                alias: None,
                            });
                        }
                        "aliased_import" => {
                            // поле name = импортируемый путь, поле alias
                            let module = child
                                .child_by_field_name("name")
                                .map(text_of)
                                .unwrap_or_default();
                            let alias = child
                                .child_by_field_name("alias")
                                .map(text_of)
                                .unwrap_or_default();
                            if !module.is_empty() {
                                push_py_import_edge(out, file_node_id, &module);
                                out.imports.push(crate::project::ast::ImportRef {
                                    rel_path: rel_path.to_string(),
                                    module,
                                    item: None,
                                    alias: if alias.is_empty() { None } else { Some(alias) },
                                });
                            }
                        }
                        _ => {}
                    }
                    // Вложенные dotted_name внутри aliased_import — повторный
                    // обход не нужен: дети не эмитятся отдельно.
                }
            }
            "import_from_statement" => {
                let module = node
                    .child_by_field_name("module_name")
                    .map(text_of)
                    .unwrap_or_default();
                if !module.is_empty() {
                    push_py_import_edge(out, file_node_id, &module);
                    // элементы: dotted_name / aliased_import (кроме module_name)
                    let mut c = node.walk();
                    for child in node.children(&mut c) {
                        let item_node = match child.kind() {
                            "dotted_name" => child,
                            "aliased_import" => match child.child_by_field_name("name") {
                                Some(n) => n,
                                None => continue,
                            },
                            _ => continue,
                        };
                        if item_node.id() == node
                            .child_by_field_name("module_name")
                            .map(|m| m.id())
                            .unwrap_or(0)
                        {
                            continue;
                        }
                        let item = text_of(item_node);
                        if item.is_empty() {
                            continue;
                        }
                        let alias = if child.kind() == "aliased_import" {
                            child
                                .child_by_field_name("alias")
                                .map(text_of)
                                .filter(|s| !s.is_empty())
                        } else {
                            None
                        };
                        out.imports.push(crate::project::ast::ImportRef {
                            rel_path: rel_path.to_string(),
                            module: module.clone(),
                            item: Some(item),
                            alias,
                        });
                        break; // первый элемент достаточно для type-pass зеркала
                    }
                }
            }
            "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let class_name = text_of(name_node);
                    if !class_name.is_empty() {
                        let node_id = format!("class:{rel_path}:{class_name}");
                        let line = node.start_position().row + 1;
                        // Базы — argument_list (superclasses) child
                        let mut base_cursor = node.walk();
                        let bases_text = node
                            .children(&mut base_cursor)
                            .find(|c| c.kind() == "argument_list")
                            .map(|b| b.utf8_text(src).unwrap_or("").to_string())
                            .unwrap_or_default();
                        let inner = bases_text
                            .trim_start_matches('(')
                            .trim_end_matches(')')
                            .trim();
                        let decos = decorators_of(node, src);
                        let kind_suffix = if inner.contains("BaseModel") || inner.contains("BaseSettings") {
                            " (Pydantic-модель)"
                        } else if decos.iter().any(|d| d.contains("dataclass")) {
                            " (dataclass)"
                        } else {
                            ""
                        };
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: class_name.clone(),
                            node_type: "Class".to_string(),
                            description: format!("Класс `{class_name}` в {rel_path}{kind_suffix}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
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
                        // INHERITS: базы из argument_list (голые имена — как regex)
                        if !inner.is_empty() {
                            for base in inner.split(',') {
                                let base_clean = base.trim();
                                if !base_clean.is_empty() {
                                    out.edges.push(AstEdge {
                                        source_node_id: node_id.clone(),
                                        target_node_id: format!("class:{base_clean}"),
                                        label: "INHERITS".to_string(),
                                        weight: 1.0,
                                        context: format!("extends {base_clean}"),
                                        provenance: "EXTRACTED".to_string(),
                                    });
                                }
                            }
                        }
                        // QUERIES_TABLE: __tablename__ внутри тела класса (Ф40.2)
                        collect_py_tablename(node, src, &node_id, out);
                    }
                }
            }
            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let fn_name = text_of(name_node);
                    if !fn_name.is_empty() {
                        let node_id = format!("fn:{rel_path}:{fn_name}");
                        let decos = decorators_of(node, src);
                        let description = if decos.is_empty() {
                            format!("Функция/метод `{fn_name}` в {rel_path}")
                        } else {
                            format!(
                                "Функция/метод `{fn_name}` в {rel_path}. Декораторы: {}",
                                decos.join(" ")
                            )
                        };
                        let line = node.start_position().row + 1;
                        let line_end = node
                            .child_by_field_name("body")
                            .map(|b| b.end_position().row + 1)
                            .unwrap_or(line);
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: fn_name.clone(),
                            node_type: "Function".to_string(),
                            description,
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line_end.max(line),
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
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
                        // 71.2: pytest-конвенция — test_* функции
                        if fn_name.starts_with("test_") {
                            out.test_node_ids.push(node_id.clone());
                        }
                        // ROUTE: декораторный блок перед def (окно i-2..i)
                        let row0 = name_node.start_position().row;
                        let route = [row0.checked_sub(2), row0.checked_sub(1), Some(row0)]
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
            "call" => {
                // 71.1: function — identifier (прямые) или attribute.attribute
                // (obj.method()); 71.4: qualifier — простой object идентификатор
                if let Some(f) = node.child_by_field_name("function") {
                    let (name, qualifier) = match f.kind() {
                        "identifier" => (text_of(f), None),
                        "attribute" => {
                            let name = f
                                .child_by_field_name("attribute")
                                .map(|n| text_of(n))
                                .unwrap_or_default();
                            let q = f
                                .child_by_field_name("object")
                                .filter(|o| o.kind() == "identifier")
                                .map(|o| text_of(o));
                            (name, q)
                        }
                        _ => (String::new(), None),
                    };
                    if !name.is_empty() {
                        out.call_sites.push(CallSite {
                            rel_path: rel_path.to_string(),
                            line: node.start_position().row + 1,
                            name,
                            qualifier,
                        });
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                walk_python(child, src, rel_path, file_node_id, route_by_line, out);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }

    /// IMPORTS-ребро file → module:{name} (формат regex-бэкенда).
    fn push_py_import_edge(out: &mut AstScanResult, file_node_id: &str, module: &str) {
        out.edges.push(AstEdge {
            source_node_id: file_node_id.to_string(),
            target_node_id: format!("module:{module}"),
            label: "IMPORTS".to_string(),
            weight: 1.0,
            context: format!("import {module}"),
            provenance: "EXTRACTED".to_string(),
        });
    }

    /// QUERIES_TABLE: `__tablename__ = "x"` в теле класса (Ф40.2, INFERRED).
    /// Присваивание лежит внутри block-внука — обход поддерево рекурсивно.
    fn collect_py_tablename(
        class_node: Node,
        src: &[u8],
        class_node_id: &str,
        out: &mut AstScanResult,
    ) {
        visit_py_tablename(class_node, src, class_node_id, out);
    }

    fn visit_py_tablename(
        node: Node,
        src: &[u8],
        class_node_id: &str,
        out: &mut AstScanResult,
    ) {
        if node.kind() == "assignment"
            && node
                .child_by_field_name("left")
                .map(|l| l.utf8_text(src).unwrap_or("") == "__tablename__")
                .unwrap_or(false)
        {
            if let Some(right) = node.child_by_field_name("right") {
                let text = right.utf8_text(src).unwrap_or("");
                let tbl = text.trim_matches(|c| c == '"' || c == '\'');
                if !tbl.is_empty() {
                    out.edges.push(AstEdge {
                        source_node_id: class_node_id.to_string(),
                        target_node_id: format!("table:{tbl}"),
                        label: "QUERIES_TABLE".to_string(),
                        weight: 1.0,
                        context: format!("__tablename__ = \"{tbl}\""),
                        provenance: "INFERRED".to_string(),
                    });
                }
            }
        }
        let mut c = node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                visit_py_tablename(child, src, class_node_id, out);
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }

    /// Ф70.6 (PLAN_v2.0): CST→AST маппинг Go (tree-sitter-go).
    /// Зеркало regex-бэкенда (структуры/интерфейсы как Struct, функции/методы)
    /// плюс IMPORTS-рёбра, которые regex-бэкенд для Go не извлекал (superset);
    /// call_sites — для эталона вызовов (golden `target`), type-pass резолвит.
    pub fn map_go(rel_path: &str, content: &str, root: Node, out: &mut AstScanResult) {
        let file_node_id = push_file_node(out, rel_path, content);
        let src = content.as_bytes();
        walk_go(root, src, rel_path, &file_node_id, out);
        collect_call_sites_standalone(rel_path, content, crate::project::ast::GO_CALL_SKIP, out);
        out.call_sites.sort();
        out.call_sites.dedup();
    }

    fn walk_go(node: Node, src: &[u8], rel_path: &str, file_node_id: &str, out: &mut AstScanResult) {
        let text_of = |n: Node| n.utf8_text(src).unwrap_or("").to_string();
        match node.kind() {
            "import_declaration" => {
                // import_spec: поле path — интерпретируемый строковый литерал
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() == "import_spec" {
                        if let Some(path_node) = child.child_by_field_name("path") {
                            let p = path_node.utf8_text(src).unwrap_or("").trim_matches('"');
                            if !p.is_empty() {
                                out.edges.push(AstEdge {
                                    source_node_id: file_node_id.to_string(),
                                    target_node_id: format!("module:{p}"),
                                    label: "IMPORTS".to_string(),
                                    weight: 1.0,
                                    context: format!("import \"{p}\";"),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        }
                    }
                }
            }
            "type_spec" => {
                // type X struct {...} / type X interface {...} — оба как Struct
                // (зеркало go_struct_re: «Go структура/интерфейс»)
                if let (Some(name_node), Some(ty)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("type"),
                ) {
                    if matches!(ty.kind(), "struct_type" | "interface_type") {
                        let name = text_of(name_node);
                        if !name.is_empty() {
                            let node_id = format!("struct:{rel_path}:{name}");
                            let line = node.start_position().row + 1;
                            out.nodes.push(AstNode {
                                node_id: node_id.clone(),
                                label: name.clone(),
                                node_type: "Struct".to_string(),
                                description: format!(
                                    "Go структура/интерфейс `{name}` в {rel_path}"
                                ),
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
            }
            "function_declaration" | "method_declaration" => {
                // method_declaration: имя после receiver — regex-бэкенд эмитил
                // методы тем же `fn:{rel}:{name}`; зеркалим.
                if let Some(name_node) = node.child_by_field_name("name") {
                    let fn_name = text_of(name_node);
                    if !fn_name.is_empty() {
                        let node_id = format!("fn:{rel_path}:{fn_name}");
                        let line = name_node.start_position().row + 1;
                        let line_end = node
                            .child_by_field_name("body")
                            .map(|b| b.end_position().row + 1)
                            .unwrap_or(line);
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: fn_name.clone(),
                            node_type: "Function".to_string(),
                            description: format!("Go функция/метод `{fn_name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line_end.max(line),
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
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
                         // 71.2: go-тесты — Test* функции верхнего уровня
                         if fn_name.starts_with("Test") {
                             out.test_node_ids.push(node_id.clone());
                         }
                     }
                 }
             }
            "call_expression" => {
                // 71.1: function — identifier или selector_expression.sel
                // (pkg.Func / obj.Method); 71.4: qualifier — операнд селектора
                if let Some(f) = node.child_by_field_name("function") {
                    let (name, qualifier) = match f.kind() {
                        "identifier" => (text_of(f), None),
                        "selector_expression" => {
                            let name = f
                                .child_by_field_name("sel")
                                .map(|n| text_of(n))
                                .unwrap_or_default();
                            let q = f
                                .child_by_field_name("operand")
                                .filter(|o| o.kind() == "identifier")
                                .map(|o| text_of(o));
                            (name, q)
                        }
                        _ => (String::new(), None),
                    };
                    if !name.is_empty() {
                        out.call_sites.push(CallSite {
                            rel_path: rel_path.to_string(),
                            line: node.start_position().row + 1,
                            name,
                            qualifier,
                        });
                    }
                }
            }
             _ => {}
         }
         let mut c = node.walk();
         if c.goto_first_child() {
             loop {
                 let child = c.node();
                 walk_go(child, src, rel_path, file_node_id, out);
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }

    /// Ф70.12 (PLAN_v2.0): CST→AST маппинг Dart (tree-sitter-dart, nielsenko).
    /// Зеркало regex-бэкенда: классы (extends → INHERITS, with → IMPLEMENTS
    /// trait:, implements → IMPLEMENTS interface:), миксины как Trait,
    /// функции/методы; IMPORTS из uri. Первая очередь v2.0 — эталон обязателен.
    pub fn map_dart(rel_path: &str, content: &str, root: Node, out: &mut AstScanResult) {
        let file_node_id = push_file_node(out, rel_path, content);
        let src = content.as_bytes();
        walk_dart(root, src, rel_path, &file_node_id, out);
        collect_call_sites_standalone(rel_path, content, crate::project::ast::DART_CALL_SKIP, out);
        out.call_sites.sort();
        out.call_sites.dedup();
    }

    fn walk_dart(
        node: Node,
        src: &[u8],
        rel_path: &str,
        file_node_id: &str,
        out: &mut AstScanResult,
    ) {
        let text_of = |n: Node| n.utf8_text(src).unwrap_or("").to_string();
        match node.kind() {
            "import_or_export" => {
                if let Some(uri) = find_descendant(node, "configurable_uri") {
                    let raw = text_of(uri);
                    let module = raw.trim_matches(|c| c == '\'' || c == '"');
                    if !module.is_empty() {
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: format!("module:{module}"),
                            label: "IMPORTS".to_string(),
                            weight: 1.0,
                            context: format!("import '{module}';"),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                }
            }
            "class_declaration" => {
                if let Some(name_node) = first_direct_child(node, "identifier") {
                    let class_name = text_of(name_node);
                    if !class_name.is_empty() {
                        let node_id = format!("class:{rel_path}:{class_name}");
                        let line = node.start_position().row + 1;
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: class_name.clone(),
                            node_type: "Class".to_string(),
                            description: format!("Dart класс `{class_name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
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
                        // Грамматика nielsenko: mixins вложены В superclass
                        // (superclass = extends Base with Mix), interfaces — прямой
                        // ребёнок. extends → INHERITS class:{}, with → IMPLEMENTS
                        // trait:{}, implements → IMPLEMENTS interface:{}.
                        let mut emit = |prefix: &str, verb: &str, label: &str, ids: &[String]| {
                            for id in ids {
                                out.edges.push(AstEdge {
                                    source_node_id: node_id.clone(),
                                    target_node_id: format!("{prefix}:{id}"),
                                    label: label.to_string(),
                                    weight: 1.0,
                                    context: format!("{verb} {id}"),
                                    provenance: "EXTRACTED".to_string(),
                                });
                            }
                        };
                        if let Some(superclass) = first_direct_child(node, "superclass") {
                            emit("class", "extends", "INHERITS", &direct_type_texts(superclass, src));
                            if let Some(mixins) = first_direct_child(superclass, "mixins") {
                                emit("trait", "with", "IMPLEMENTS", &direct_type_texts(mixins, src));
                            }
                        }
                        if let Some(interfaces) = first_direct_child(node, "interfaces") {
                            emit("interface", "implements", "IMPLEMENTS", &direct_type_texts(interfaces, src));
                        }
                    }
                }
            }
            "mixin_declaration" => {
                if let Some(name_node) = first_direct_child(node, "identifier") {
                    let mixin_name = text_of(name_node);
                    if !mixin_name.is_empty() {
                        let node_id = format!("trait:{rel_path}:{mixin_name}");
                        let line = node.start_position().row + 1;
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: mixin_name.clone(),
                            node_type: "Trait".to_string(),
                            description: format!("Dart миксин `{mixin_name}` в {rel_path}"),
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
            "function_declaration" | "method_declaration" => {
                // имя — первый identifier внутри function_signature
                let name = find_descendant(node, "function_signature")
                    .and_then(|sig| first_direct_child(sig, "identifier"))
                    .map(|n| n.utf8_text(src).unwrap_or("").to_string());
                if let Some(fn_name) = name {
                    if !fn_name.is_empty() {
                        let node_id = format!("fn:{rel_path}:{fn_name}");
                        let line = node.start_position().row + 1;
                        let line_end = find_descendant(node, "function_body")
                            .map(|b| b.end_position().row + 1)
                            .unwrap_or(line);
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: fn_name.clone(),
                            node_type: "Function".to_string(),
                            description: format!("Dart функция/метод `{fn_name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line_end.max(line),
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
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
                        // 71.2: dart-тесты — все функции *_test.dart
                        if rel_path.ends_with("_test.dart") {
                            out.test_node_ids.push(node_id.clone());
                        }
                    }
                }
            }
            "method_invocation" => {
                // 71.1: поле method — прямые вызовы и obj.method() (цепочки
                // a.b().c() дают каждое звено); конструкторы без поля пропускаем.
                // 71.4: qualifier — target-приёмник (простой идентификатор)
                if let Some(m) = node.child_by_field_name("method") {
                    let name = text_of(m);
                    let qualifier = node
                        .child_by_field_name("target")
                        .filter(|t| t.kind() == "simple_identifier")
                        .map(|t| text_of(t));
                    if !name.is_empty() {
                        out.call_sites.push(CallSite {
                            rel_path: rel_path.to_string(),
                            line: node.start_position().row + 1,
                            name,
                            qualifier,
                        });
                    }
                }
            }
            _ => {}
        }
        let mut c = node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                walk_dart(child, src, rel_path, file_node_id, out);
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }

    /// Ф70.4 (PLAN_v2.0): CST→AST маппинг TS/JS (tree-sitter-typescript/tsx).
    /// Зеркало regex-бэкенда: import/class/interface/type-alias/function;
    /// superset: методы классов (method_definition) и стрелки
    /// `const x = (...) =>` — regex их терял. Мосты/эвристики (HTTP, gql,
    /// React BINDS_CLASS) — те же regex-проходы, что у regex-бэкенда,
    /// чтобы фича не теряла функциональность на ts-файлах.
    pub fn map_ts_js(rel_path: &str, content: &str, root: Node, out: &mut AstScanResult) {
        let file_node_id = push_file_node(out, rel_path, content);
        let src = content.as_bytes();
        walk_ts(root, src, rel_path, &file_node_id, out);
        // Мосты и эвристики — без регрессии относительно regex-бэкенда
        crate::project::ast::collect_http_calls_standalone(rel_path, content, out);
        crate::project::ast::collect_gql_fields_standalone(rel_path, content, out);
        crate::project::ast::collect_ts_binds_class(content, &file_node_id, out);
        collect_call_sites_standalone(rel_path, content, crate::project::ast::TS_CALL_SKIP, out);
        out.call_sites.sort();
        out.call_sites.dedup();
    }

    fn walk_ts(
        node: Node,
        src: &[u8],
        rel_path: &str,
        file_node_id: &str,
        out: &mut AstScanResult,
    ) {
        let text_of = |n: Node| n.utf8_text(src).unwrap_or("").to_string();
        match node.kind() {
            "import_statement" => {
                if let Some(source_node) = node.child_by_field_name("source") {
                    let raw = text_of(source_node);
                    let module = raw.trim_matches(|c| c == '\'' || c == '"');
                    if !module.is_empty() {
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: format!("module:{module}"),
                            label: "IMPORTS".to_string(),
                            weight: 1.0,
                            context: format!("import from '{module}'"),
                            provenance: "EXTRACTED".to_string(),
                        });
                        // 71.4: ImportRef — named/namespace/default импорты
                        // для file-scope таблицы импортов (type-pass).
                        // Структура TS-грамматики: import_clause →
                        // named_imports → import_specifier (вложенность!),
                        // namespace_import и голый identifier (default).
                        let mut push_ref = |item: Option<String>, alias: Option<String>| {
                            out.imports.push(crate::project::ast::ImportRef {
                                rel_path: rel_path.to_string(),
                                module: module.to_string(),
                                item,
                                alias,
                            });
                        };
                        let mut c = node.walk();
                        for child in node.children(&mut c) {
                            if child.kind() != "import_clause" {
                                continue;
                            }
                            let mut cc = child.walk();
                            let clause_children: Vec<_> = child.children(&mut cc).collect();
                            for spec in clause_children {
                                match spec.kind() {
                                    "import_specifier" => {
                                        let item = spec
                                            .child_by_field_name("name")
                                            .map(text_of)
                                            .unwrap_or_default();
                                        let alias = spec
                                            .child_by_field_name("alias")
                                            .map(text_of)
                                            .filter(|s| !s.is_empty());
                                        if !item.is_empty() {
                                            push_ref(Some(item), alias);
                                        }
                                    }
                                    "named_imports" => {
                                        let mut c3 = spec.walk();
                                        let inner: Vec<_> = spec.children(&mut c3).collect();
                                        for ispec in inner {
                                            if ispec.kind() != "import_specifier" {
                                                continue;
                                            }
                                            let item = ispec
                                                .child_by_field_name("name")
                                                .map(text_of)
                                                .unwrap_or_default();
                                            let alias = ispec
                                                .child_by_field_name("alias")
                                                .map(text_of)
                                                .filter(|s| !s.is_empty());
                                            if !item.is_empty() {
                                                push_ref(Some(item), alias);
                                            }
                                        }
                                    }
                                    "namespace_import" => {
                                        // `import * as Utils from './utils'`;
                                        // имя — единственный named-потомок
                                        let by_field = spec
                                            .child_by_field_name("name")
                                            .map(text_of)
                                            .filter(|s| !s.is_empty());
                                        let alias = by_field.or_else(|| {
                                            let mut c4 = spec.walk();
                                            let found = spec
                                                .children(&mut c4)
                                                .find(|n| n.kind() == "identifier")
                                                .map(|n| text_of(n));
                                            found
                                        });
                                        if let Some(alias) = alias {
                                            push_ref(None, Some(alias));
                                        }
                                    }
                                    "identifier" => {
                                        // default-импорт: `import Def from './def'`
                                        push_ref(None, Some(text_of(spec)));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            "class_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text_of(name_node);
                    if !name.is_empty() {
                        let node_id = format!("class:{rel_path}:{name}");
                        let line = node.start_position().row + 1;
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: name.clone(),
                            node_type: "Class".to_string(),
                            description: format!("TypeScript/JS класс `{name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
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
                        // 71.2: implements → IMPLEMENTS к interface:{name} (голые)
                        if let Some(heritage) = first_direct_child(node, "class_heritage") {
                            let ht = heritage.utf8_text(src).unwrap_or("");
                            if let Some(pos) = ht.find("implements") {
                                for part in ht[pos + "implements".len()..].split(',') {
                                    let iface = part.trim();
                                    if !iface.is_empty()
                                        && iface
                                            .chars()
                                            .all(|c| c.is_alphanumeric() || c == '_')
                                    {
                                        out.edges.push(AstEdge {
                                            source_node_id: node_id.clone(),
                                            target_node_id: format!("interface:{iface}"),
                                            label: "IMPLEMENTS".to_string(),
                                            weight: 1.0,
                                            context: format!("implements {iface}"),
                                            provenance: "EXTRACTED".to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "interface_declaration" | "type_alias_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text_of(name_node);
                    if !name.is_empty() {
                        let node_id = format!("interface:{rel_path}:{name}");
                        let line = node.start_position().row + 1;
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: name.clone(),
                            node_type: "Interface".to_string(),
                            description: format!("Интерфейс/тип `{name}` в {rel_path}"),
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
            "function_declaration" | "method_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let fn_name = text_of(name_node);
                    if !fn_name.is_empty() {
                        let node_id = format!("fn:{rel_path}:{fn_name}");
                        let line = name_node.start_position().row + 1;
                        let line_end = node
                            .child_by_field_name("body")
                            .map(|b| b.end_position().row + 1)
                            .unwrap_or(line);
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: fn_name.clone(),
                            node_type: "Function".to_string(),
                            description: format!("Функция `{fn_name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line_end.max(line),
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: node_id.clone(),
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
                        // 71.2: js/ts-конвенция — test*/it* функции
                        if fn_name.starts_with("test") {
                            out.test_node_ids.push(node_id.clone());
                        }
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                // superset: const/let/var x = (...) => ... — стрелочные функции
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() != "variable_declarator" {
                        continue;
                    }
                    let is_arrow = child
                        .child_by_field_name("value")
                        .map(|v| v.kind() == "arrow_function")
                        .unwrap_or(false);
                    if !is_arrow {
                        continue;
                    }
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let fn_name = text_of(name_node);
                        if fn_name.is_empty() {
                            continue;
                        }
                        let node_id = format!("fn:{rel_path}:{fn_name}");
                        let line = name_node.start_position().row + 1;
                        out.nodes.push(AstNode {
                            node_id,
                            label: fn_name.clone(),
                            node_type: "Function".to_string(),
                            description: format!("Функция `{fn_name}` в {rel_path}"),
                            file_path: rel_path.to_string(),
                            line_start: line,
                            line_end: line,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.to_string(),
                            target_node_id: format!("fn:{rel_path}:{fn_name}"),
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
            "call_expression" => {
                // 71.1: function — identifier или member_expression.property
                // (obj.method()); property_identifier/private_property_identifier.
                // 71.4: qualifier — простой object-идентификатор
                if let Some(f) = node.child_by_field_name("function") {
                    let (name, qualifier) = match f.kind() {
                        "identifier" => (text_of(f), None),
                        "member_expression" => {
                            let name = f
                                .child_by_field_name("property")
                                .map(|n| text_of(n))
                                .unwrap_or_default();
                            let q = f
                                .child_by_field_name("object")
                                .filter(|o| o.kind() == "identifier")
                                .map(|o| text_of(o));
                            (name, q)
                        }
                        _ => (String::new(), None),
                    };
                    if !name.is_empty() {
                        out.call_sites.push(CallSite {
                            rel_path: rel_path.to_string(),
                            line: node.start_position().row + 1,
                            name,
                            qualifier,
                        });
                    }
                }
            }
            _ => {}
        }
        let mut c = node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                walk_ts(child, src, rel_path, file_node_id, out);
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }

    /// Ф70.7 (PLAN_v2.0): декларативные .scm-запросы символов (tags.scm-стиль,
    /// вшитые строки). Назначение — кросс-проверка mapper'ов против
    /// декларативных определений (тесты на эталонах Ф69.3) и основа для 71.x;
    /// боевой путь разбора — ручные обходы (provenance и поля грамматик
    /// требуют контекст, queries не дают tier'ы достоверности).
    pub fn query_symbols(root: Node, src: &[u8], lang: &str) -> Vec<(String, String)> {
        let Some(q) = symbol_query(lang) else {
            return Vec::new();
        };
        // Имена захватов → индексы (API 0.27: только capture_index_for_name)
        let idx_name = q.capture_index_for_name("name");
        let tags: Vec<(u32, &str)> = [
            ("fn", "Function"),
            ("struct", "Struct"),
            ("trait", "Trait"),
            ("class", "Class"),
            ("interface", "Interface"),
        ]
        .iter()
        .filter_map(|(n, t)| q.capture_index_for_name(n).map(|i| (i, *t)))
        .collect();
        let mut qc = tree_sitter::QueryCursor::new();
        let mut out: Vec<(String, String)> = Vec::new();
        let mut matches = qc.matches(q, root, src);
        while let Some(m) = matches.next() {
            let mut name: Option<String> = None;
            let mut kind: Option<&str> = None;
            for c in m.captures() {
                if Some(c.index) == idx_name {
                    name = Some(c.node.utf8_text(src).unwrap_or("").to_string());
                } else if let Some((_, t)) = tags.iter().find(|(i, _)| *i == c.index) {
                    kind = Some(t);
                }
            }
            if let (Some(n), Some(k)) = (name, kind) {
                out.push((n, k.to_string()));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Текст .scm-запроса TS/TSX (общий для двух грамматик).
    const TS_QUERY_TEXT: &str = "(function_declaration name: (identifier) @name) @fn
(method_definition name: (property_identifier) @name) @fn
(class_declaration name: (type_identifier) @name) @class
(interface_declaration name: (type_identifier) @name) @interface
(type_alias_declaration name: (type_identifier) @name) @interface
(variable_declarator name: (identifier) @name value: (arrow_function)) @fn";

    /// Компилированный .scm-запрос символов языка (OnceLock — один раз).
    fn symbol_query(lang: &str) -> Option<&'static tree_sitter::Query> {
        static NONE_Q: Option<tree_sitter::Query> = None;
        static RUST: std::sync::OnceLock<Option<tree_sitter::Query>> = std::sync::OnceLock::new();
        static PYTHON: std::sync::OnceLock<Option<tree_sitter::Query>> = std::sync::OnceLock::new();
        static GO: std::sync::OnceLock<Option<tree_sitter::Query>> = std::sync::OnceLock::new();
        static DART: std::sync::OnceLock<Option<tree_sitter::Query>> = std::sync::OnceLock::new();
        static TS: std::sync::OnceLock<Option<tree_sitter::Query>> = std::sync::OnceLock::new();
        static TSX: std::sync::OnceLock<Option<tree_sitter::Query>> = std::sync::OnceLock::new();
        let compile = |grammar: tree_sitter::Language, text: &str| -> Option<tree_sitter::Query> {
            tree_sitter::Query::new(&grammar, text).ok()
        };
        let q = match lang {
            "rust" => RUST.get_or_init(|| {
                compile(
                    language_for("rust")?,
                    r#"(function_item name: (identifier) @name) @fn
(function_signature_item name: (identifier) @name) @fn
(struct_item name: (type_identifier) @name) @struct
(enum_item name: (type_identifier) @name) @struct
(trait_item name: (type_identifier) @name) @trait"#,
                )
            }),
            "python" => PYTHON.get_or_init(|| {
                compile(
                    language_for("python")?,
                    "(function_definition name: (identifier) @name) @fn
(class_definition name: (identifier) @name) @class",
                )
            }),
            "go" => GO.get_or_init(|| {
                compile(
                    language_for("go")?,
                    "(function_declaration name: (identifier) @name) @fn
(method_declaration name: (field_identifier) @name) @fn
(type_spec name: (type_identifier) @name type: (struct_type)) @struct
(type_spec name: (type_identifier) @name type: (interface_type)) @struct",
                )
            }),
            "dart" => DART.get_or_init(|| {
                compile(
                    language_for("dart")?,
                    r#"(function_declaration . (function_signature (identifier) @name)) @fn
(method_declaration . (method_signature (function_signature (identifier) @name))) @fn
(class_declaration "class" (identifier) @name) @class
(mixin_declaration "mixin" (identifier) @name) @trait"#,
                )
            }),
            "ts" => TS.get_or_init(|| compile(language_for("ts")?, TS_QUERY_TEXT)),
            "tsx" => TSX.get_or_init(|| compile(language_for("tsx")?, TS_QUERY_TEXT)),
            _ => &NONE_Q,
        };
        q.as_ref()
    }
}
/// Создаётся один раз на скан (конструирование regex'ов дорогое).
pub struct AstParserDispatcher {
    regex: RegexBackend,
    #[cfg(feature = "treesitter")]
    ts_enabled: bool,
    #[cfg(feature = "treesitter")]
    incremental: ts::IncrementalParser,
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

/// Ф70.15 (PLAN_v2.0): маски генерированного кода для stubbing —
/// файлы, для которых извлекаются только верхнеуровневые символы
/// (вывод build_runner/protobuf раздувает граф в 3–4 раза).
pub fn is_generated_stub(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    lower.ends_with(".g.dart")
        || lower.ends_with(".freezed.dart")
        || lower.ends_with(".pb.dart")
        || lower.ends_with(".pb.rs")
        || lower.ends_with(".generated.ts")
}

impl AstParserDispatcher {
    pub fn new() -> Self {
        Self {
            regex: RegexBackend::new(),
            #[cfg(feature = "treesitter")]
            ts_enabled: Self::ts_enabled(),
            #[cfg(feature = "treesitter")]
            incremental: ts::IncrementalParser::new(),
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
        // 70.15: генерированный код — только символы верхнего уровня (через
        // .scm-запросы 70.7), без тел/вызовов/мостов/ROUTE — иначе вывод
        // build_runner/protobuf раздувает граф и поиск.
        #[cfg(feature = "treesitter")]
        if is_generated_stub(rel_path) {
            return self.parse_stub(rel_path, content, out);
        }
        #[cfg(feature = "treesitter")]
        if self.ts_enabled {
            // 71.5: SFC (Vue/Svelte) — гибрид: regex-мосты (STYLES/BINDS_CLASS/
            // template-IMPORTS + regex-скрипт) + tree-sitter разбор `<script>`
            // с дедупом по node_id (CST superset: стрелки/методы/вызовы)
            if rel_path.ends_with(".vue") || rel_path.ends_with(".svelte") {
                return self.parse_sfc_hybrid(rel_path, content, out);
            }
            if let Some(lang) = lang_for_path(rel_path) {
                if has_mapper(lang) {
                    // 70.8: инкрементальный парсинг (LRU-кэш деревьев + Tree::edit);
                    // 70.9: ошибка (в т.ч. доля ERROR > порога) — fallback на regex
                    if let Ok(tree) = self.incremental.parse_tree(rel_path, content, lang) {
                        ts::map_tree(lang, rel_path, content, tree.root_node(), out);
                        return format!("treesitter:{lang}");
                    }
                }
            }
        }
        self.regex.parse_source(rel_path, content, out);
        "regex".to_string()
    }

    /// 71.5: гибридный разбор Single File Component (Vue/Svelte): полный
    /// regex-проход (мосты Ф48.3: STYLES/BINDS_CLASS/template-IMPORTS и
    /// regex-разбор скрипта) + tree-sitter разбор каждого `<script>`-блока
    /// через TS-грамматику со смещением строк; дедуп по node_id (CST-символы
    /// — superset regex: стрелки/методы; одинаковые node_id не дублируются).
    /// Ошибка tree-sitter — тихий fallback на regex-результат (70.9).
    #[cfg(feature = "treesitter")]
    fn parse_sfc_hybrid(&self, rel_path: &str, content: &str, out: &mut AstScanResult) -> String {
        // 1. Полный regex-проход (все мосты + regex-скрипт)
        self.regex.parse_source(rel_path, content, out);

        // 2. tree-sitter поверх: каждый <script>-блок через TS-грамматику.
        // Скрипт-блоки не кэшируются в LRU (это часть файла, а не файл).
        let spans = self.regex.extractor().sfc_script_spans(content);
        if spans.is_empty() {
            return "treesitter:sfc".to_string();
        }
        let mut seen_nodes: std::collections::HashSet<String> = out
            .nodes
            .iter()
            .map(|n| n.node_id.clone())
            .collect();
        let edge_key = |e: &AstEdge| format!("{}|{}|{}", e.source_node_id, e.target_node_id, e.label);
        let mut seen_edges: std::collections::HashSet<String> =
            out.edges.iter().map(&edge_key).collect();

        for (offset, script) in spans {
            let mut block = AstScanResult::default();
            if ts::parse_source(rel_path, &script, "ts", &mut block).is_err() {
                continue; // 70.9: fallback — остаётся regex-результат
            }
            // смещение строк: символы и места вызовов — относительно всего SFC
            for mut n in block.nodes {
                n.line_start += offset;
                n.line_end += offset;
                if seen_nodes.insert(n.node_id.clone()) {
                    out.nodes.push(n);
                }
            }
            out.edges.extend(
                block
                    .edges
                    .into_iter()
                    .filter(|e| seen_edges.insert(edge_key(e))),
            );
            for mut c in block.call_sites {
                c.line += offset;
                out.call_sites.push(c);
            }
            out.imports.extend(block.imports);
            out.test_node_ids.extend(block.test_node_ids);
        }
        let before = out.call_sites.len();
        out.call_sites.sort();
        out.call_sites.dedup();
        let _ = before;
        "treesitter:sfc".to_string()
    }

    /// Ф70.15: stub-режим генерированного кода — символы из .scm-запроса
    /// (все 5 языков одним механизмом), без call_sites/мостов/ROUTE.
    /// Ошибка tree-sitter → regex (v1.9-поведение, деградации нет).
    #[cfg(feature = "treesitter")]
    fn parse_stub(&self, rel_path: &str, content: &str, out: &mut AstScanResult) -> String {
        if let Some(lang) = lang_for_path(rel_path) {
            if has_mapper(lang) {
                if let Ok(tree) = self.incremental.parse_tree(rel_path, content, lang) {
                    let file_node_id = ts::push_file_node(out, rel_path, content);
                    let src = content.as_bytes();
                    for (name, kind) in ts::query_symbols(tree.root_node(), src, lang) {
                        let prefix = match kind.as_str() {
                            "Function" => "fn",
                            "Struct" => "struct",
                            "Trait" => "trait",
                            "Class" => "class",
                            _ => "interface",
                        };
                        let node_id = format!("{prefix}:{rel_path}:{name}");
                        out.nodes.push(AstNode {
                            node_id: node_id.clone(),
                            label: name.clone(),
                            node_type: kind,
                            description: format!(
                                "Символ генерированного кода `{name}` в {rel_path}"
                            ),
                            file_path: rel_path.to_string(),
                            line_start: 1,
                            line_end: 1,
                        });
                        out.edges.push(AstEdge {
                            source_node_id: file_node_id.clone(),
                            target_node_id: node_id,
                            label: "DEFINES".to_string(),
                            weight: 1.0,
                            context: String::new(),
                            provenance: "EXTRACTED".to_string(),
                        });
                    }
                    return format!("treesitter:stub:{lang}");
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

/// Ф70.14 (PLAN_v2.0): канонический SCIP-подобный дескриптор символа —
/// целевая схема node_id для миграции M12 (71.3): `{lang}:crate/{module}#{owner}#{name}{suffix}`,
/// suffix — `()` для вызываемых, `.` для типов (формат примера плана:
/// `rust:crate/module/Struct#method().`). package — плейсхолдер `crate` до
/// чтения manifest'ов проектов (71.x). Переключение node_id на эту схему
/// рвёт привязку памяти/edges без переиндексации — только в составе M12 (71.3).
pub fn scip_descriptor(
    lang: &str,
    rel_path: &str,
    owner: Option<&str>,
    name: &str,
    callable: bool,
) -> String {
    // module path: rel_path без расширения ("src/api.rs" → "src/api")
    let module = rel_path
        .rsplit_once('.')
        .filter(|(_, ext)| ext.len() <= 4)
        .map(|(base, _)| base)
        .unwrap_or(rel_path);
    let suffix = if callable { "()" } else { "." };
    match owner {
        Some(owner) => format!("{lang}:crate/{module}/{owner}#{name}{suffix}"),
        None => format!("{lang}:crate/{module}#{name}{suffix}"),
    }
}

#[cfg(test)]
mod scip_tests {
    use super::scip_descriptor;

    #[test]
    fn scip_descriptor_formats() {
        // Пример из плана: rust:crate/module/Struct#method().
        assert_eq!(
            scip_descriptor("rust", "src/module.rs", Some("Struct"), "method", true),
            "rust:crate/src/module/Struct#method()"
        );
        // Тип (не вызываемый)
        assert_eq!(
            scip_descriptor("rust", "src/api.rs", None, "CacheConfig", false),
            "rust:crate/src/api#CacheConfig."
        );
        // Остальные языки
        assert_eq!(
            scip_descriptor("python", "src/views.py", None, "run_pipeline", true),
            "python:crate/src/views#run_pipeline()"
        );
        assert_eq!(
            scip_descriptor("go", "src/store.go", Some("Store"), "Get", true),
            "go:crate/src/store/Store#Get()"
        );
        assert_eq!(
            scip_descriptor("dart", "lib/model.dart", None, "TokenCache", false),
            "dart:crate/lib/model#TokenCache."
        );
        assert_eq!(
            scip_descriptor("ts", "src/api.ts", None, "runPipeline", true),
            "ts:crate/src/api#runPipeline()"
        );
    }

    #[test]
    fn scip_descriptor_deterministic() {
        let a = scip_descriptor("rust", "src/a.rs", Some("S"), "m", true);
        let b = scip_descriptor("rust", "src/a.rs", Some("S"), "m", true);
        assert_eq!(a, b);
        assert_ne!(
            scip_descriptor("rust", "src/a.rs", None, "m", true),
            scip_descriptor("rust", "src/b.rs", None, "m", true)
        );
        assert_ne!(
            scip_descriptor("rust", "src/a.rs", None, "m", true),
            scip_descriptor("rust", "src/a.rs", None, "n", true)
        );
    }
}
