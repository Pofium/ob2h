//! Ф24: int8-квантование, dual-read, CLI quantize-embeddings, backup quick/verify.

use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

use tempfile::tempdir;

use ob2h::cli::db::run_quantize;
use ob2h::config::Settings;
use ob2h::vector::{cosine, deserialize, serialize, serialize_q};
use ob2h::{init_app, mcp::AppContext};

#[tokio::test]
async fn quantize_cli_converts_legacy_and_verify_detects_corruption() {
    let tmp = tempdir().expect("tempdir");
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    let ctx: Arc<AppContext> = init_app(settings).expect("init app");

    // Писатели теперь v2 — подкладываем легаси f32-блоб вручную
    ctx.memory
        .save("Факт для квантования", Some("hmem-q"), "t", 0.5, "chat", None)
        .await
        .expect("save");
    ctx.db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET embedding = ?1 WHERE key = 'hmem-q'",
                rusqlite::params![serialize(&vec![0.3f32; 8])],
            )?;
            Ok(())
        })
        .expect("seed legacy blob");

    // dry-run ничего не меняет
    run_quantize(&ctx, true).expect("dry run");
    let before: Vec<u8> = ctx
        .db
        .with_conn(|conn| {
            conn.query_row("SELECT embedding FROM memories WHERE key = 'hmem-q'", [], |r| r.get(0))
        })
        .expect("read before");
    assert_eq!(before.len(), 32, "dry-run: блоб остался легаси f32");

    // конвертация
    run_quantize(&ctx, false).expect("quantize");
    let after: Vec<u8> = ctx
        .db
        .with_conn(|conn| {
            conn.query_row("SELECT embedding FROM memories WHERE key = 'hmem-q'", [], |r| r.get(0))
        })
        .expect("read after");
    assert_eq!(after.len(), 8 + 5, "v2 = dim + 5");
    let restored = deserialize(&after).expect("dual-read v2");
    assert!((cosine(&[0.3f32; 8], &restored) - 1.0).abs() < 0.01);

    // повторный прогон — квантовать нечего
    run_quantize(&ctx, false).expect("idempotent");

    // quick-бэкап + verify
    let quick = ctx.backup.create_quick().expect("quick backup");
    let report = ctx.backup.verify(&quick).expect("verify ok");
    assert!(report.contains("integrity_check: ok"), "{report}");
    assert!(report.contains("memories:"), "{report}");
    assert!(!report.contains("graph_nodes:"), "в quick-бэкапе графа нет: {report}");

    // порченная копия детектится
    let corrupted = tmp.path().join("corrupted.db");
    std::fs::copy(quick.join("ob2h-quick.db"), &corrupted).expect("copy");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&corrupted)
        .expect("open corrupted");
    f.seek(SeekFrom::Start(100)).expect("seek");
    f.write_all(&[0xFFu8; 64]).expect("corrupt");
    drop(f);
    let bad_report = ctx.backup.verify(&corrupted).expect("verify report");
    assert!(
        !bad_report.contains("integrity_check: ok"),
        "порча должна быть видна: {bad_report}"
    );
}

#[tokio::test]
async fn full_backup_verify_matches_live_counts() {
    let tmp = tempdir().expect("tempdir");
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    let ctx = init_app(settings).expect("init app");

    ctx.memory
        .save("Для полного бэкапа", Some("hmem-full"), "t", 0.5, "chat", None)
        .await
        .expect("save");
    let target = ctx.backup.create().expect("full backup");
    let report = ctx.backup.verify(&target).expect("verify");
    assert!(report.contains("integrity_check: ok"), "{report}");
    assert!(report.contains("graph_nodes:"), "{report}");
    assert!(!report.contains("⚠"), "расхождений быть не должно: {report}");
}
