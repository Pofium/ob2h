//! CLI-интерфейс на базе clap.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ob2h", author, version, about = "Локальное MCP-хранилище знаний для Hermes на Rust", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Запустить MCP stdio сервер (поведение по умолчанию)
    Serve,
    /// Управление процессом дриминга памяти
    Dream {
        #[command(subcommand)]
        command: DreamCommands,
    },
    /// Создать атомарный бэкап БД и workspace
    Backup,
    /// Вывести статистику хранилища
    Stats,
    /// Установить и зарегистрировать OB2H в Hermes (config.yaml)
    Install,
    /// Удалить OB2H из Hermes (config.yaml)
    Uninstall,
    /// Управление MemoryProvider-плагином ob2h для Hermes
    Plugin {
        #[command(subcommand)]
        command: PluginCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum PluginCommands {
    /// Установить плагин в $HERMES_HOME/plugins/ob2h (конфиг Hermes не правится)
    Install,
    /// Удалить плагин из $HERMES_HOME/plugins/ob2h
    Uninstall,
    /// Проверить установку плагина и активность в конфиге Hermes
    Status,
}

#[derive(Subcommand, Debug)]
pub enum DreamCommands {
    /// Запустить дриминг вручную
    Run {
        #[arg(short, long)]
        background: bool,
    },
    /// Проверить статус дрима и гейты автодрима
    Status,
    /// Просмотреть историю dream-коммитов
    Log {
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Откатить файлы памяти к указанному коммиту
    Restore {
        #[arg(short, long)]
        commit: String,
    },
}
