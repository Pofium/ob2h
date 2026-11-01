//! Ф62.3: E2EE-шифрование синхронизационных бандлов (Argon2id + XChaCha20-Poly1305).
//! Отдельный бинарник тестов: переменная окружения OB2H_SYNC_KEY изолирована
//! от остальных sync-тестов.

use ob2h::config::Settings;
use ob2h::init_app;
use ob2h::mcp::AppContext;
use tempfile::tempdir;

fn make_ctx(dir: &std::path::Path, origin: &str) -> std::sync::Arc<AppContext> {
    let sync_dir = dir.join("sync");
    std::fs::create_dir_all(&sync_dir).unwrap();
    std::fs::write(
        sync_dir.join("peers.json"),
        format!("{{\"origin\": \"{origin}\"}}\n"),
    )
    .unwrap();
    let mut settings = Settings::from_env();
    settings.data_dir = dir.to_path_buf();
    settings.embed_provider = "fake".to_string();
    init_app(settings).expect("init_app")
}

#[tokio::test]
async fn test_e2ee_bundle_roundtrip() {
    std::env::set_var("OB2H_SYNC_KEY", "correct horse battery staple");
    let dir_pc = tempdir().unwrap();
    let dir_vps = tempdir().unwrap();
    let pc = make_ctx(dir_pc.path(), "pc");
    let vps = make_ctx(dir_vps.path(), "vps");

    pc.memory
        .save("секретный факт про проект", Some("secret"), "g", 0.9, "chat", None)
        .await
        .unwrap();
    let bundle = pc.sync.export("vps").unwrap();

    // Файл — не gzip, а контейнер OB2HENC1; плейнтекст в байтах не виден.
    let bytes = std::fs::read(&bundle).unwrap();
    assert!(bytes.starts_with(b"ob2henc1"), "бандл должен быть зашифрован");
    assert!(
        !String::from_utf8_lossy(&bytes).contains("секретный факт"),
        "плейнтекст не должен утекать в бандл"
    );

    // Импорт тем же ключом — успешен, факт доехал.
    let stats = vps.sync.import_file(&bundle).await.unwrap();
    assert_eq!(stats.memories_applied, 1);
    assert!(vps.memory.get("secret").unwrap().is_some());

    // Импорт без ключа — ошибка (расшифровка невозможна).
    std::env::remove_var("OB2H_SYNC_KEY");
    let dir_no_key = tempdir().unwrap();
    let no_key = make_ctx(dir_no_key.path(), "vps2");
    assert!(
        no_key.sync.import_file(&bundle).await.is_err(),
        "без ключа импорт зашифрованного бандла должен падать"
    );

    // Импорт с неверным ключом — ошибка целостности AEAD.
    std::env::set_var("OB2H_SYNC_KEY", "definitely wrong key");
    let dir_wrong = tempdir().unwrap();
    let wrong = make_ctx(dir_wrong.path(), "vps3");
    assert!(
        wrong.sync.import_file(&bundle).await.is_err(),
        "неверный ключ должен проваливать расшифровку"
    );

    std::env::remove_var("OB2H_SYNC_KEY");
}
