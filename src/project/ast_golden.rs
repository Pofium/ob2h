//! Ф69.3 (PLAN_v2.0): `ast_golden` — эталонные деревья символов и вызовов по языкам.
//! Фикстуры: `tests/fixtures/ast_golden/<lang>/` (`source.<ext>` + `expected.json`).
//!
//! Эталон кодирует **семантику языка** (что корректный разбор обязан найти),
//! а не причуды текущего regex-парсера — смена бэкенда на tree-sitter (Фаза 70)
//! валидируется против этих же файлов (гейты 70.11 / 71.7: symbol F1 ≥ 0.95,
//! call F1 ≥ 0.90). `call_extraction: "target"` означает, что рёбра вызовов —
//! цель для tree-sitter (сейчас call_sites собираются только для Rust/Python).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Требуемый символ эталона: имя + словарный node_type реестра LANGS.
#[derive(Debug, Deserialize)]
pub struct AstGoldenSymbol {
    pub name: String,
    pub kind: String,
}

/// Требуемое ребро вызова: `from` → `to` (имена узлов-функций).
#[derive(Debug, Deserialize)]
pub struct AstGoldenCall {
    pub from: String,
    pub to: String,
}

/// Спецификация эталона одного языка.
#[derive(Debug, Deserialize)]
pub struct AstGolden {
    pub language: String,
    /// Имя файла-фикстуры внутри каталога `<dir>/<language>/`.
    pub fixture: String,
    /// `current` — CALLS-рёбра обязаны находиться текущим парсером;
    /// `target` — цель для tree-sitter (Фаза 70), сейчас не проверяется.
    pub call_extraction: String,
    pub required_symbols: Vec<AstGoldenSymbol>,
    pub required_calls: Vec<AstGoldenCall>,
    #[serde(default)]
    pub notes: String,
}

/// Загрузить все эталоны из каталога: подкаталог = язык, в нём `expected.json`
/// и файл-фикстура (`fixture` из спецификации). Возвращает пары
/// (полный путь к фикстуре, спецификация). Каталоги сортируются по имени —
/// детерминированный порядок прогона.
pub fn load_all(dir: &Path) -> anyhow::Result<Vec<(PathBuf, AstGolden)>> {
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();
    for lang_dir in entries {
        let spec_path = lang_dir.join("expected.json");
        let text = std::fs::read_to_string(&spec_path).map_err(|e| {
            anyhow::anyhow!("{}: нет expected.json ({e})", lang_dir.display())
        })?;
        let golden: AstGolden = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{}: {e}", spec_path.display()))?;
        if golden.language != lang_dir.file_name().and_then(|s| s.to_str()).unwrap_or("") {
            anyhow::bail!(
                "{}: language `{}` не совпадает с именем каталога",
                spec_path.display(),
                golden.language
            );
        }
        if !matches!(golden.call_extraction.as_str(), "current" | "target") {
            anyhow::bail!(
                "{}: call_extraction должен быть current|target, получено `{}`",
                spec_path.display(),
                golden.call_extraction
            );
        }
        if golden.required_symbols.is_empty() {
            anyhow::bail!("{}: required_symbols пуст", spec_path.display());
        }
        let fixture_path = lang_dir.join(&golden.fixture);
        if !fixture_path.is_file() {
            anyhow::bail!("{}: фикстура не найдена", fixture_path.display());
        }
        out.push((fixture_path, golden));
    }
    if out.is_empty() {
        anyhow::bail!("{}: ни одного эталона не найдено", dir.display());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ast_golden")
    }

    #[test]
    fn load_all_reads_all_five_languages() {
        let items = load_all(&fixtures_dir()).expect("загрузка ast_golden");
        let langs: Vec<&str> = items
            .iter()
            .map(|(_, g)| g.language.as_str())
            .collect();
        for lang in ["dart", "go", "python", "rust", "ts"] {
            assert!(langs.contains(&lang), "нет эталона для {lang}");
        }
    }

    #[test]
    fn load_all_bails_on_missing_expected() {
        let dir = tempfile::tempdir().unwrap();
        let lang = dir.path().join("rust");
        std::fs::create_dir_all(&lang).unwrap();
        assert!(load_all(dir.path()).is_err());
    }
}
