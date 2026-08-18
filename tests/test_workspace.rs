use ob2h::workspace::{GitStore, Workspace};
use tempfile::tempdir;

#[test]
fn test_workspace_files_and_history() {
    let tmp = tempdir().expect("tempdir");
    let ws = Workspace::new(tmp.path());

    ws.write_file("soul", "Ты — личный помощник Hermes.").expect("write soul");
    let read_soul = ws.read_file("soul").expect("read soul");
    assert_eq!(read_soul, "Ты — личный помощник Hermes.");

    let c1 = ws.append_history("Консолидированный факт 1").expect("append 1");
    let c2 = ws.append_history("Консолидированный факт 2").expect("append 2");
    assert_eq!(c1, 1);
    assert_eq!(c2, 2);

    let entries = ws.read_history_from_cursor(0, 10).expect("read history");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].content, "Консолидированный факт 1");

    let cursor = ws.get_cursor().expect("get_cursor").unwrap();
    assert_eq!(cursor, 2);

    ws.compact_history(1).expect("compact");
    let entries_compacted = ws.read_history_from_cursor(0, 10).expect("read compacted");
    assert_eq!(entries_compacted.len(), 1);
    assert_eq!(entries_compacted[0].content, "Консолидированный факт 2");
}

#[test]
fn test_git_store_auto_commit() {
    let tmp = tempdir().expect("tempdir");
    let ws = Workspace::new(tmp.path());
    let git = GitStore::new(tmp.path());

    ws.write_file("soul", "Версия 1").expect("write soul");
    let sha = git.auto_commit("Initial commit");
    if sha.is_some() {
        let logs = git.log(5);
        assert!(!logs.is_empty());
        assert_eq!(logs[0].message, "Initial commit");
    }
}
