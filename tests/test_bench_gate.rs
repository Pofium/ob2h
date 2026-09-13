//! Ф30 (PLAN_v1.4): ночной bench-гейт дрима. Без сети: FakeEmbedding,
//! in-memory БД, tempdir workspace. Гейт включается явно (дефолт off).

use std::sync::Arc;

use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::dream::bench_gate::{is_degradation, read_history, run_gate};
use ob2h::embedding::FakeEmbedding;
use ob2h::memory::MemoryService;
use ob2h::workspace::GitStore;
use tempfile::tempdir;

fn settings(tmp: &tempfile::TempDir, timeout_ms: u64) -> Settings {
    let mut s = Settings::from_env();
    s.data_dir = tmp.path().to_path_buf();
    s.bench_gate_enabled = true;
    s.bench_gate_timeout_ms = timeout_ms;
    s
}

struct Env {
    settings: Settings,
    memory: Arc<MemoryService>,
    db: Database,
    gitstore: Arc<GitStore>,
}

fn setup(timeout_ms: u64) -> (tempfile::TempDir, Env) {
    setup_with(timeout_ms, Arc::new(FakeEmbedding::new(384)))
}

fn setup_with(timeout_ms: u64, embedder: Arc<dyn ob2h::embedding::EmbeddingProvider>) -> (tempfile::TempDir, Env) {
    let tmp = tempdir().expect("tempdir");
    let settings = settings(&tmp, timeout_ms);
    let db = Database::in_memory().expect("db");
    let memory = Arc::new(MemoryService::new(db.clone(), embedder));
    let gitstore = Arc::new(GitStore::new(tmp.path().join("workspace")));
    std::fs::create_dir_all(tmp.path().join("workspace")).expect("ws dir");
    (tmp, Env { settings, memory, db, gitstore })
}

/// FakeEmbedding с реальным sleep на embed: даёт bench'у настоящую точку
/// Pending — иначе прогон завершается синхронно в одном poll и tokio
/// timeout не срабатывает даже с нулевым бюджетом.
struct SlowEmbedding {
    inner: Arc<FakeEmbedding>,
}

impl SlowEmbedding {
    fn new(inner: Arc<FakeEmbedding>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl ob2h::embedding::EmbeddingProvider for SlowEmbedding {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        self.inner.embed(texts).await
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }
}

fn write_golden(tmp: &tempfile::TempDir, keys: &[&str]) {
    let dir = tmp.path().join("bench");
    std::fs::create_dir_all(&dir).expect("bench dir");
    let keys_json: Vec<String> = keys.iter().map(|k| format!("\"{k}\"")).collect();
    let line = format!(
        "{{\"query\":\"какой кофе любит пользователь\",\"expect_keys\":[{}],\"note\":\"t\"}}\n",
        keys_json.join(",")
    );
    std::fs::write(dir.join("golden.jsonl"), line).expect("golden");
}

fn seed_bench_last(db: &Database, recall5: f64, mrr: f64) {
    db.set_kv(
        "bench:last",
        &serde_json::json!({ "recall5": recall5, "mrr": mrr }).to_string(),
    )
    .expect("kv");
}

fn bench_last(db: &Database) -> Option<(f64, f64)> {
    let raw = db.get_kv("bench:last").expect("kv get")?;
    let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
    Some((v["recall5"].as_f64()?, v["mrr"].as_f64()?))
}

// --- Чистая логика порогов -------------------------------------------------

#[test]
fn degradation_thresholds() {
    // recall@5: относительное падение > 10% — деградация
    assert!(is_degradation((0.9, 0.9), (0.7, 0.9)));
    // ровно 10% — ещё не деградация
    assert!(!is_degradation((1.0, 1.0), (0.9, 1.0)));
    // MRR: падение 20% при стабильном recall — тоже деградация (dual-сигнал)
    assert!(is_degradation((0.8, 0.5), (0.8, 0.4)));
    // MRR: 15% и меньше — не деградация
    assert!(!is_degradation((0.8, 0.2), (0.8, 0.17)));
    // обе метрики стабильны
    assert!(!is_degradation((0.5, 0.5), (0.5, 0.5)));
    // нулевой bench:last — делить нельзя, деградации нет
    assert!(!is_degradation((0.0, 0.0), (0.0, 0.0)));
    // рост метрик — не деградация
    assert!(!is_degradation((0.4, 0.4), (0.9, 0.9)));
}

// --- Workspace-инвариант (ловит «дрим съел память») ------------------------

#[test]
fn workspace_integrity_detects_wipe_only() {
    let tmp = tempdir().expect("tempdir");
    let git = GitStore::new(tmp.path());

    let healthy: String = (1..=10).map(|i| format!("факт {i}\n")).collect();
    std::fs::write(tmp.path().join("SOUL.md"), &healthy).expect("write");
    let sha = git.auto_commit("init").expect("commit");

    // точечные правки (±2 строки) — не испорчено
    std::fs::write(tmp.path().join("SOUL.md"), healthy.replace("факт 1", "факт 1 (актуально)"))
        .expect("write");
    assert_eq!(git.file_shrunk_beyond(&sha, 0.5), None);

    // обвал до 2 строк из 10 — испорчено
    std::fs::write(tmp.path().join("SOUL.md"), "мусор\nещё мусор\n").expect("write");
    assert_eq!(git.file_shrunk_beyond(&sha, 0.5), Some("SOUL.md".to_string()));

    // файла вообще нет — тоже испорчено
    std::fs::remove_file(tmp.path().join("SOUL.md")).expect("rm");
    assert_eq!(git.file_shrunk_beyond(&sha, 0.5), Some("SOUL.md".to_string()));
}

// --- Гейт -------------------------------------------------------------------

#[tokio::test]
async fn gate_skipped_without_golden() {
    let (_tmp, env) = setup(3000);
    let verdict = run_gate(
        &env.memory, &env.db, &env.gitstore, &env.settings, Some("abc1234"), None,
    )
    .await
    .expect("verdict");
    assert_eq!(verdict.outcome, "skipped");
    assert!(verdict.reason.as_deref().unwrap_or("").contains("golden"));
    // skipped-прогон не пишет метрики в bench:last
    assert!(bench_last(&env.db).is_none());
}

#[tokio::test]
async fn first_run_adopts_then_healthy_run_passes() {
    let (tmp, env) = setup(5000);
    write_golden(&tmp, &["k-coffee"]);
    env.memory
        .save("Пользователь пьёт кофе утром", Some("k-coffee"), "preferences", 0.5, "chat", None)
        .await
        .expect("save");

    let v1 = run_gate(&env.memory, &env.db, &env.gitstore, &env.settings, None, None)
        .await
        .expect("verdict1");
    assert_eq!(v1.outcome, "first_run");
    assert!(bench_last(&env.db).is_some());

    let v2 = run_gate(&env.memory, &env.db, &env.gitstore, &env.settings, None, None)
        .await
        .expect("verdict2");
    assert_eq!(v2.outcome, "pass", "здоровый дрим: метрики не изменились");

    // счётчик прогонов для Ф35.2 и две строки истории
    let runs: i64 = env.db.get_kv("bench:runs").expect("kv").unwrap().parse().unwrap();
    assert_eq!(runs, 2);
    let history = read_history(&env.settings, 10).expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["gate"], "first_run");
    assert_eq!(history[1]["gate"], "pass");
}

#[tokio::test]
async fn degradation_rolls_back_workspace_and_keeps_bench_last() {
    let (tmp, env) = setup(5000);
    write_golden(&tmp, &["k-coffee"]);

    // Здоровый workspace: 10 строк, коммит — точка отката.
    let ws = tmp.path().join("workspace");
    let healthy: String = (1..=10).map(|i| format!("- знание {i}\n")).collect();
    std::fs::write(ws.join("SOUL.md"), &healthy).expect("write");
    let prev_sha = env.gitstore.auto_commit("до дрима").expect("commit");

    // Дрим-правка: слегка ужала файл (40% < порога инварианта) — но метрики
    // просядут, потому что golden-ключ исчез из БД (tombstone).
    let shrunk: String = (1..=6).map(|i| format!("- знание {i}\n")).collect();
    std::fs::write(ws.join("SOUL.md"), &shrunk).expect("write");

    env.memory
        .save("Пользователь пьёт кофе утром", Some("k-coffee"), "preferences", 0.5, "chat", None)
        .await
        .expect("save");
    env.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET deleted_at = '2026-09-13T00:00:00+00:00' WHERE key = 'k-coffee'",
                [],
            )?;
            Ok(())
        })
        .expect("tombstone");
    seed_bench_last(&env.db, 1.0, 1.0);

    let verdict = run_gate(
        &env.memory, &env.db, &env.gitstore, &env.settings, Some("feedc0de"), Some(&prev_sha),
    )
    .await
    .expect("verdict");

    assert_eq!(verdict.outcome, "rollback", "recall 1.0 → 0.0 обязан откатить");
    assert_eq!(verdict.restored_to.as_deref(), Some(prev_sha.as_str()));
    let alert = verdict.alert.expect("алерт в дрим-отчёт");
    assert!(alert.contains("ОТКАТ"), "алерт: {alert}");
    assert!(alert.contains("feedc0de"), "в алерте sha дрима: {alert}");

    // workspace восстановлен на предыдущий коммит (git checkout на Windows
    // возвращает CRLF — сравниваем с нормализацией переводов строк)
    let after = std::fs::read_to_string(ws.join("SOUL.md")).expect("read");
    let norm = |s: &str| s.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n");
    assert_eq!(norm(&after), norm(&healthy), "SOUL.md возвращён к состоянию до дрима");

    // bench:last НЕ обновляется при откате
    assert_eq!(bench_last(&env.db), Some((1.0, 1.0)));

    // история с sha дрима и исходом rollback
    let history = read_history(&env.settings, 10).expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["gate"], "rollback");
    assert_eq!(history[0]["dream_sha"], "feedc0de");
    assert_eq!(history[0]["embedding_backend"], env.settings.embed_provider);
}

#[tokio::test]
async fn timeout_skips_without_rollback() {
    let (tmp, env) = setup_with(1, Arc::new(SlowEmbedding::new(Arc::new(FakeEmbedding::new(384)))));
    write_golden(&tmp, &["k-coffee"]);
    env.memory
        .save("Пользователь пьёт кофе утром", Some("k-coffee"), "preferences", 0.5, "chat", None)
        .await
        .expect("save");

    let ws = tmp.path().join("workspace");
    std::fs::write(ws.join("SOUL.md"), "- строка\n").expect("write");
    seed_bench_last(&env.db, 0.9, 0.9);

    let verdict = run_gate(&env.memory, &env.db, &env.gitstore, &env.settings, Some("aaaa"), None)
        .await
        .expect("verdict");

    // Timeout ≠ rollback: warning, гейт пропущен, bench:last не трогается
    assert_eq!(verdict.outcome, "timeout");
    assert_eq!(bench_last(&env.db), Some((0.9, 0.9)));
    assert_eq!(
        std::fs::read_to_string(ws.join("SOUL.md")).expect("read"),
        "- строка\n",
        "никакого отката"
    );
    let history = read_history(&env.settings, 10).expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["gate"], "timeout");
}

#[tokio::test]
async fn gate_disabled_returns_none() {
    let (tmp, mut env) = setup(3000);
    env.settings.bench_gate_enabled = false; // дефолт: off до набора статистики
    write_golden(&tmp, &["k-coffee"]);
    let verdict = run_gate(
        &env.memory, &env.db, &env.gitstore, &env.settings, None, None,
    )
    .await;
    assert!(verdict.is_none(), "гейт выключен — воркер ничего не делает");
}
