//! 71.8 (PLAN_v2.0): граф совместных изменений (Co-Change / Temporal Coupling).
//!
//! AST не видит межъязыковых скрытых связей (Rust-хендлер ↔ SQL-миграция ↔
//! Dart-клиент API ↔ `.env.example`). Проход по последним ~200 коммитам
//! (`git log --name-only`): пары файлов, менявшихся вместе ≥ `MIN_CO_CHANGES`
//! раз в компактных коммитах (≤ `MAX_COMMIT_FILES` файлов), получают ребро
//! `CO_CHANGES_WITH` между file-узлами (схема `file:`, 70.14) с
//! `provenance = GIT_HISTORY` и confidence по частоте совстречаемости.
//! Инкрементальная достройка при каждом скане — идемпотентный upsert.

use std::collections::HashMap;
use std::path::Path;

/// Сколько последних коммитов смотреть.
pub const MAX_COMMITS: usize = 200;

/// Коммиты шире этой границы (bulk-рефакторинги, переименования) не считаются
/// «компактными» — совместное изменение в них неинформативно.
pub const MAX_COMMIT_FILES: usize = 10;

/// Минимум совместных изменений пары для ребра.
pub const MIN_CO_CHANGES: u32 = 3;

/// Одна пара файлов, менявшихся вместе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoChangePair {
    /// rel_path первого файла (канонический порядок a < b).
    pub a: String,
    /// rel_path второго файла.
    pub b: String,
    /// Сколько раз встретились вместе в компактных коммитах.
    pub count: u32,
}

impl CoChangePair {
    /// Confidence по частоте совстречаемости: 3 → 0.5, 4 → 0.6, …, ≥8 → 1.0.
    /// Нижняя граница 0.5 — единая шкала с эвристиками (HEURISTIC/INFERRED),
    /// чтобы co-change-рёбра проходили дефолтный фильтр `min_confidence`
    /// в `project_impact` (71.6).
    pub fn confidence(&self) -> f64 {
        (0.5 + 0.1 * (self.count - MIN_CO_CHANGES) as f64).min(1.0)
    }
}

/// Есть ли git-репозиторий в корне проекта (быстрая проверка `.git`).
pub fn is_git_repo(root: &Path) -> bool {
    root.join(".git").exists()
}

/// Собрать пары совместных изменений по истории git.
/// Чистая функция над выводом `git log` — легко тестируется.
/// Ошибки git (не репозиторий, git не установлен) → пустой список, не Err:
/// co-change — необязательное обогащение, скан не должен падать.
pub fn compute_pairs(root: &Path) -> Vec<CoChangePair> {
    compute_pairs_with(root, MAX_COMMITS, MAX_COMMIT_FILES, MIN_CO_CHANGES)
}

/// Вариант с настраиваемыми порогами (тесты).
pub fn compute_pairs_with(
    root: &Path,
    max_commits: usize,
    max_commit_files: usize,
    min_count: u32,
) -> Vec<CoChangePair> {
    // verbatim-префикс канонизации Windows (\\?\C:\...) git не понимает
    let root_str = root.to_string_lossy().replace('\\', "/");
    let root_str = root_str
        .strip_prefix("//?/")
        .map(|s| s.to_string())
        .unwrap_or(root_str);
    let out = std::process::Command::new("git")
        .args([
            "-C",
            &root_str,
            "log",
            "--name-only",
            // %x00 — git сам вставляет NUL-разделитель в ПОТОК ВЫВОДА; настоящий
            // NUL в argv на Windows не передаётся (CreateProcess-ограничение)
            "--pretty=format:%x00%H",
            "-n",
            &max_commits.to_string(),
        ])
        .output();
    let Ok(output) = out else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_log_and_count(&text, max_commit_files, min_count)
}

/// Разобрать вывод `git log --name-only --pretty=format:\0%H` и посчитать пары.
/// Разделение по NUL — имена файлов могут содержать что угодно, кроме NUL.
fn parse_log_and_count(text: &str, max_commit_files: usize, min_count: u32) -> Vec<CoChangePair> {
    let mut pair_counts: HashMap<(String, String), u32> = HashMap::new();
    for chunk in text.split('\u{0}') {
        let chunk = chunk.trim_start_matches(['\r', '\n']);
        if chunk.is_empty() {
            continue;
        }
        // первая строка — хеш коммита, дальше — файлы коммита
        let mut lines = chunk.lines().filter(|l| !l.trim().is_empty());
        let Some(_hash) = lines.next() else {
            continue;
        };
        let mut files: Vec<String> = lines
            .map(|l| l.trim().replace('\\', "/"))
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        // некомпактный коммит (bulk-рефакторинг) — совместное изменение
        // в нём неинформативно
        if files.len() < 2 || files.len() > max_commit_files {
            continue;
        }
        let sorted: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        for i in 0..sorted.len() {
            for j in (i + 1)..sorted.len() {
                *pair_counts
                    .entry((sorted[i].to_string(), sorted[j].to_string()))
                    .or_default() += 1;
            }
        }
    }
    let mut pairs: Vec<CoChangePair> = pair_counts
        .into_iter()
        .filter(|(_, c)| c >= &min_count)
        .map(|((a, b), count)| CoChangePair { a, b, count })
        .collect();
    pairs.sort_by(|x, y| y.count.cmp(&x.count).then_with(|| x.a.cmp(&y.a)));
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фиктивный вывод git log: пара a-b встречалась 3 раза (компактные),
    /// пара c-d — 2 раза (ниже порога), bulk-коммит из 12 файлов не считается.
    #[test]
    fn parse_log_counts_compact_pairs() {
        let mut log = String::new();
        for _ in 0..3 {
            log.push_str("\u{0}abc123\nsrc/a.rs\nsrc/b.rs\n");
        }
        log.push_str("\u{0}def456\nsrc/c.rs\nsrc/d.rs\n");
        log.push_str("\u{0}def457\nsrc/c.rs\nsrc/d.rs\n");
        // некомпактный bulk-коммит: a и b вместе ещё раз, но файлов 12
        let mut bulk = String::from("\u{0}bulk789\n");
        for i in 0..12 {
            bulk.push_str(&format!("f{i}.rs\n"));
        }
        bulk.push_str("src/a.rs\nsrc/b.rs\n");
        log.push_str(&bulk);

        let pairs = parse_log_and_count(&log, MAX_COMMIT_FILES, MIN_CO_CHANGES);
        assert_eq!(pairs.len(), 1, "только пара a-b: {pairs:?}");
        assert_eq!(pairs[0].a, "src/a.rs");
        assert_eq!(pairs[0].b, "src/b.rs");
        assert_eq!(pairs[0].count, 3);
        assert!((pairs[0].confidence() - 0.5).abs() < 1e-9);
    }

    /// Парa с большим числом совстречаемости даёт более высокий confidence.
    #[test]
    fn confidence_grows_with_count() {
        let c3 = CoChangePair {
            a: "a".into(),
            b: "b".into(),
            count: 3,
        };
        let c8 = CoChangePair {
            a: "a".into(),
            b: "b".into(),
            count: 8,
        };
        let c99 = CoChangePair {
            a: "a".into(),
            b: "b".into(),
            count: 99,
        };
        assert!((c3.confidence() - 0.5).abs() < 1e-9);
        assert!((c8.confidence() - 1.0).abs() < 1e-9);
        assert_eq!(c99.confidence(), c8.confidence(), "кап 1.0");
    }

    /// Живой git-репозиторий: 4 коммита с парой файлов, пороги адаптированы.
    #[test]
    fn live_git_repo_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn b() {}\n").unwrap();
        std::fs::write(root.join("c.rs"), "fn c() {}\n").unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-C", &root.to_string_lossy()])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        for i in 0..4 {
            std::fs::write(root.join("a.rs"), format!("fn a() {{ {} }}\n", i)).unwrap();
            std::fs::write(root.join("b.rs"), format!("fn b() {{ {} }}\n", i)).unwrap();
            run(&["add", "a.rs", "b.rs"]);
            run(&["commit", "-q", "-m", &format!("c{i}")]);
        }
        // c.rs меняется один — пары с ним не должно быть
        std::fs::write(root.join("c.rs"), "fn c() { 1 }\n").unwrap();
        run(&["add", "c.rs"]);
        run(&["commit", "-q", "-m", "solo"]);

        let pairs = compute_pairs_with(root, MAX_COMMITS, MAX_COMMIT_FILES, 3);
        assert_eq!(pairs.len(), 1, "только a-b: {pairs:?}");
        assert_eq!(pairs[0].a, "a.rs");
        assert_eq!(pairs[0].b, "b.rs");
        assert_eq!(pairs[0].count, 4);
        assert!(is_git_repo(root));

        // не репозиторий — пусто, без паники
        let empty = tempfile::tempdir().unwrap();
        assert!(compute_pairs(empty.path()).is_empty());
    }
}
