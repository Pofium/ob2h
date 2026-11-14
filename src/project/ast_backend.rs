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

/// Языки с реализованным CST→AST-маппингом в `AstScanResult`.
/// До Ф70.3 список пуст — диспетчер уводит всё в regex.
#[cfg(feature = "treesitter")]
pub fn has_mapper(lang: &str) -> bool {
    let _ = lang;
    false
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
    use tree_sitter::Parser;

    use crate::project::ast::AstScanResult;

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

    /// Разобрать файл tree-sitter'ом и смаппить в `AstScanResult`.
    /// Маппинг по языкам — Ф70.3 (rust), Ф70.4 (ts/js), Ф70.5 (python),
    /// Ф70.6 (go), Ф70.12 (dart); до реализации соответствующего маппера
    /// возвращается Err — диспетчер уходит в regex (70.9).
    pub fn parse_source(
        rel_path: &str,
        content: &str,
        lang: &str,
        // `_out` — маппинг CST → AstScanResult реализуется в Ф70.3+;
        // до этого параметр не используется, файл уходит в regex (70.9).
        _out: &mut AstScanResult,
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
        // 70.9: регламент fallback — ERROR-узлы в CST > порога уводят в regex.
        // Здесь пока проверяется только «нет ошибок вовсе»; долевой порог — 70.9.
        if root.has_error() {
            anyhow::bail!("CST `{rel_path}` содержит ERROR-узлы — fallback на regex");
        }
        // Маппинг CST → узлы/рёбра ещё не реализован (Ф70.3+) —
        // диспетчер уводит файл в regex.
        Err(anyhow::anyhow!("маппинг `{lang}` ещё не реализован (Ф70.3+)"))
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
        !ts_enabled_from_env(std::env::var("OB2H_TREESITTER").ok())
    }

    /// Прозрачный разбор файла: tree-sitter при поддержке языка (фича собрана,
    /// kill-switch не запрещает, маппер реализован), иначе regex (70.9).
    /// Возвращает имя использованного бэкенда — телеметрия.
    pub fn parse_source(&self, rel_path: &str, content: &str, out: &mut AstScanResult) -> String {
        #[cfg(feature = "treesitter")]
        if self.ts_enabled {
            if let Some(lang) = lang_for_path(rel_path) {
                if has_mapper(lang) {
                    match ts::parse_source(rel_path, content, lang, out) {
                        Ok(()) => return format!("treesitter:{lang}"),
                        Err(_) => {} // 70.9: fallback на regex
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

    /// Диспетчер в дефолтной сборке всегда regex и даёт те же узлы,
    /// что и прямой AstCodeExtractor (паритет).
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
        assert_eq!(backend, "regex");
        let ids: Vec<&str> = r.nodes.iter().map(|n| n.node_id.as_str()).collect();
        let ids_direct: Vec<&str> = direct.nodes.iter().map(|n| n.node_id.as_str()).collect();
        assert_eq!(ids, ids_direct, "диспетчер должен повторять regex-бэкенд");
    }
}
