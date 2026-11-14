//! Ф69.3 (PLAN_v2.0): `contradiction_golden` — пары фактов для валидации
//! детектора конфликтов (Фаза 74): строгое противоречие (strict), уточнение
//! (refinement), контекстное различие (coexistence). Фикстура —
//! `tests/fixtures/contradiction_golden.jsonl` (синтетическая, без персональных
//! данных); персональный набор может лежать в `data/bench/contradiction.jsonl`.

use std::path::Path;

use serde::Deserialize;

/// Допустимые типы пар эталона.
pub const CONTRADICTION_KINDS: [&str; 3] = ["strict", "refinement", "coexistence"];

/// Одна пара эталона: фрагмент A против фрагмента B.
#[derive(Debug, Deserialize)]
pub struct ContradictionPair {
    pub id: String,
    /// `strict` — взаимоисключающие факты об одном объекте;
    /// `refinement` — уточнение/новая редакция (SUPERSEDES);
    /// `coexistence` — верны одновременно в разных контекстах.
    pub kind: String,
    pub a: String,
    pub b: String,
    /// true — детектор обязан сработать (strict/refinement),
    /// false — обязан промолчать (coexistence, в т.ч. похожие тексты о разных объектах).
    pub expect_conflict: bool,
    #[serde(default)]
    pub note: String,
}

/// Загрузить пары из JSONL (пустые строки пропускаются) с валидацией инвариантов:
/// kind из словаря; strict/refinement → expect_conflict = true; coexistence → false.
pub fn load_contradiction_golden(path: &Path) -> anyhow::Result<Vec<ContradictionPair>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let pair: ContradictionPair = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("строка {}: {e}", i + 1))?;
        if !CONTRADICTION_KINDS.contains(&pair.kind.as_str()) {
            anyhow::bail!(
                "строка {}: kind `{}` вне словаря {CONTRADICTION_KINDS:?}",
                i + 1,
                pair.kind
            );
        }
        if pair.a.trim().is_empty() || pair.b.trim().is_empty() {
            anyhow::bail!("строка {}: пустые фрагменты a/b", i + 1);
        }
        let expect = matches!(pair.kind.as_str(), "strict" | "refinement");
        if pair.expect_conflict != expect {
            anyhow::bail!(
                "строка {}: kind `{}` требует expect_conflict={expect}",
                i + 1,
                pair.kind
            );
        }
        out.push(pair);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contradiction_golden.jsonl")
    }

    #[test]
    fn load_fixture_and_kinds() {
        let pairs = load_contradiction_golden(&fixture()).expect("загрузка golden");
        assert!(pairs.len() >= 10, "набор слишком мал: {}", pairs.len());
        for kind in ["strict", "refinement", "coexistence"] {
            assert!(
                pairs.iter().any(|p| p.kind == kind),
                "нет пар типа {kind}"
            );
        }
        assert!(pairs.iter().all(|p| p.a != p.b), "a == b");
    }
}
