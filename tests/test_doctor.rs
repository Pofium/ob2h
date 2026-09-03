//! Интеграционный тест диагностики `ob2h doctor` (Фаза 18).

use tempfile::tempdir;
use ob2h::cli::doctor::{Doctor, DoctorStatus};
use ob2h::config::Settings;
use ob2h::init_app;

#[test]
fn test_doctor_diagnostic_run() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let mut settings = Settings::from_env();
    settings.data_dir = tmp.path().to_path_buf();
    settings.ensure_dirs()?;

    // Инициализируем приложение (создает ob2h.db со схемой M4)
    let _ctx = init_app(settings.clone())?;

    let doctor = Doctor::new(settings, false);
    let items = doctor.run()?;

    assert!(!items.is_empty());

    // Проверяем наличие ключевых секций
    let categories: Vec<String> = items.iter().map(|i| i.category.clone()).collect();
    assert!(categories.contains(&"Файловая система".to_string()));
    assert!(categories.contains(&"База данных SQLite".to_string()));
    assert!(categories.contains(&"Эмбеддинги и Вектора".to_string()));
    assert!(categories.contains(&"AI-Агенты (MCP)".to_string()));

    // Проверяем, что проверка БД прошла успешно
    let db_item = items.iter().find(|i| i.name == "Целостность базы (quick_check)").unwrap();
    assert_eq!(db_item.status, DoctorStatus::Ok);
    assert_eq!(db_item.details, "ok");

    let wal_item = items.iter().find(|i| i.name == "Режим журнала WAL").unwrap();
    assert_eq!(wal_item.status, DoctorStatus::Ok);

    Ok(())
}
