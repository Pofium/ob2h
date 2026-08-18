use std::sync::Arc;
use ob2h::config::Settings;
use ob2h::db::Database;
use ob2h::dream::Dream;
use ob2h::llm::FakeLLM;
use ob2h::workspace::{GitStore, Workspace};
use tempfile::tempdir;

#[tokio::test]
async fn test_dream_2_phase_cycle() {
    let tmp = tempdir().expect("tempdir");
    let ws = Arc::new(Workspace::new(tmp.path()));
    let git = Arc::new(GitStore::new(tmp.path()));
    let db = Database::in_memory().expect("db in memory");

    ws.write_file("memory", "- факт 1: старая информация").expect("write memory");
    ws.append_history("Пользователь обновил требования к проекту").expect("append history");

    let fake_llm = Arc::new(FakeLLM::new());
    // Ответ на фазу 1 (анализ)
    fake_llm.set_response("Новые записи истории", "Нужно заменить факт 1 на новую информацию");
    // Ответ на фазу 2 (действие edit)
    fake_llm.set_response(
        "Анализ",
        r#"{"action": "edit", "file": "memory", "old": "старая информация", "new": "актуальная информация"}"#,
    );

    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.dream_extract_enabled = false;

    let dream = Dream::new(ws.clone(), git.clone(), fake_llm, settings, db, None);
    let stats = dream.run("manual").await.expect("dream run");

    assert_eq!(stats.status, "ok");
    assert_eq!(stats.processed, 1);

    let updated_memory = ws.read_file("memory").expect("read updated memory");
    assert!(updated_memory.contains("актуальная информация"));
}
