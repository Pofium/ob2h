//! Интеграционный тест Git-хуков (Фаза 18).

use std::fs;
use tempfile::tempdir;
use ob2h::project::hooks::{generate_post_checkout_hook, generate_post_commit_hook, generate_post_merge_hook, install_git_hooks};

#[test]
fn test_generate_and_install_git_hooks() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let root = tmp.path();

    // 1. Проверяем генерацию содержимого скриптов
    let post_commit = generate_post_commit_hook("my_app");
    assert!(post_commit.contains(r#"ob2h project scan --id "my_app""#));

    let post_merge = generate_post_merge_hook("my_app");
    assert!(post_merge.contains(r#"ob2h project scan --id "my_app""#));

    let post_checkout = generate_post_checkout_hook("my_app");
    assert!(post_checkout.contains(r#"BRANCH_FLAG=$3"#));
    assert!(post_checkout.contains(r#"ob2h project scan --id "my_app""#));

    // 2. Без папки .git установка должна возвращать ошибку
    assert!(install_git_hooks(root, "my_app").is_err());

    // 3. Создаём .git и выполняем установку
    fs::create_dir_all(root.join(".git"))?;
    let installed = install_git_hooks(root, "my_app")?;
    assert_eq!(installed.len(), 3);
    assert!(installed.contains(&"post-commit".to_string()));
    assert!(installed.contains(&"post-merge".to_string()));
    assert!(installed.contains(&"post-checkout".to_string()));

    let commit_hook_path = root.join(".git").join("hooks").join("post-commit");
    let content = fs::read_to_string(&commit_hook_path)?;
    assert!(content.contains("my_app"));

    Ok(())
}
