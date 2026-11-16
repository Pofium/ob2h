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

/// 71.7: целевые пороги качества на golden-наборе.
pub const SYMBOL_F1_MIN: f64 = 0.95;
pub const CALL_F1_MIN: f64 = 0.90;

/// F1 = 2·TP / (2·TP + FP + FN); при пустых множествах — 1.0.
pub fn f1(tp: usize, fp: usize, fn_count: usize) -> f64 {
    let denom = 2 * tp + fp + fn_count;
    if denom == 0 {
        return 1.0;
    }
    2.0 * tp as f64 / denom as f64
}

/// Требуемый символ эталона: имя + словарный node_type реестра LANGS.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct AstGoldenSymbol {
    pub name: String,
    pub kind: String,
}

/// Требуемое ребро вызова: `from` → `to` (имена узлов-функций).
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct AstGoldenCall {
    pub from: String,
    pub to: String,
}

/// Спецификация эталона одного языка.
#[derive(Debug, Deserialize, serde::Serialize)]
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

/// Вшитые эталоны (compile-time `include_str!`) — `ob2h bench all` работает
/// и вне репозитория: фикстуры не требуют файловой системы.
/// Кортеж: (язык, имя файла фикстуры, исходник, expected.json).
const EMBEDDED_RAW: &[(&str, &str, &str, &str)] = &[
    (
        "dart",
        "source.dart",
        include_str!("../../tests/fixtures/ast_golden/dart/source.dart"),
        include_str!("../../tests/fixtures/ast_golden/dart/expected.json"),
    ),
    (
        "go",
        "source.go",
        include_str!("../../tests/fixtures/ast_golden/go/source.go"),
        include_str!("../../tests/fixtures/ast_golden/go/expected.json"),
    ),
    (
        "python",
        "source.py",
        include_str!("../../tests/fixtures/ast_golden/python/source.py"),
        include_str!("../../tests/fixtures/ast_golden/python/expected.json"),
    ),
    (
        "rust",
        "source.rs",
        include_str!("../../tests/fixtures/ast_golden/rust/source.rs"),
        include_str!("../../tests/fixtures/ast_golden/rust/expected.json"),
    ),
    (
        "ts",
        "source.ts",
        include_str!("../../tests/fixtures/ast_golden/ts/source.ts"),
        include_str!("../../tests/fixtures/ast_golden/ts/expected.json"),
    ),
    // Ф78 (issue #3): ассемблер — три диалекта; вызовы обязательны уже на
    // regex-волне (call_extraction: "current").
    (
        "asm-gas",
        "source.s",
        include_str!("../../tests/fixtures/ast_golden/asm-gas/source.s"),
        include_str!("../../tests/fixtures/ast_golden/asm-gas/expected.json"),
    ),
    (
        "asm-nasm",
        "source.asm",
        include_str!("../../tests/fixtures/ast_golden/asm-nasm/source.asm"),
        include_str!("../../tests/fixtures/ast_golden/asm-nasm/expected.json"),
    ),
    (
        "asm-masm",
        "source.asm",
        include_str!("../../tests/fixtures/ast_golden/asm-masm/source.asm"),
        include_str!("../../tests/fixtures/ast_golden/asm-masm/expected.json"),
    ),
];

/// Вшитый эталон: спецификация + исходник фикстуры.
pub struct EmbeddedGolden {
    pub spec: AstGolden,
    pub source: String,
}

/// Разобрать и провалидировать вшитые эталоны (те же инварианты, что в `load_all`).
pub fn load_embedded() -> anyhow::Result<Vec<EmbeddedGolden>> {
    EMBEDDED_RAW
        .iter()
        .map(|(lang, _fixture, source, spec_json)| {
            let spec: AstGolden = serde_json::from_str(spec_json)
                .map_err(|e| anyhow::anyhow!("embedded[{lang}]: expected.json: {e}"))?;
            if spec.language != *lang {
                anyhow::bail!("embedded[{lang}]: language `{}` не совпадает", spec.language);
            }
            if !matches!(spec.call_extraction.as_str(), "current" | "target") {
                anyhow::bail!(
                    "embedded[{lang}]: call_extraction `{}` вне словаря",
                    spec.call_extraction
                );
            }
            if spec.required_symbols.is_empty() {
                anyhow::bail!("embedded[{lang}]: required_symbols пуст");
            }
            Ok(EmbeddedGolden {
                spec,
                source: (*source).to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ast_golden")
    }

    #[test]
    fn load_all_reads_all_languages() {
        let items = load_all(&fixtures_dir()).expect("загрузка ast_golden");
        let langs: Vec<&str> = items
            .iter()
            .map(|(_, g)| g.language.as_str())
            .collect();
        for lang in ["dart", "go", "python", "rust", "ts", "asm-gas", "asm-nasm", "asm-masm"] {
            assert!(langs.contains(&lang), "нет эталона для {lang}");
        }
    }

    #[test]
    fn load_embedded_matches_disk_fixtures() {
        let embedded = load_embedded().expect("вшитые эталоны валидны");
        assert_eq!(embedded.len(), 8);
        let disk = load_all(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ast_golden"))
            .expect("дисковые эталоны");
        for (path, disk_spec) in &disk {
            let src_disk = std::fs::read_to_string(path).unwrap();
            let emb = embedded
                .iter()
                .find(|e| e.spec.language == disk_spec.language)
                .unwrap_or_else(|| panic!("нет вшитого эталона {}", disk_spec.language));
            assert_eq!(emb.source, src_disk, "{}: исходники разошлись", disk_spec.language);
            assert_eq!(
                serde_json::to_string(&emb.spec).unwrap(),
                serde_json::to_string(&disk_spec).unwrap(),
                "{}: спецификации разошлись",
                disk_spec.language
            );
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
