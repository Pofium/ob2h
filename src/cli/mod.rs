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
    /// Синхронизация двух инстансов ob2h (бандлы, PC ↔ VPS)
    Sync {
        #[command(subcommand)]
        command: SyncCommands,
    },
    /// Установить/обновить скилл ob2h в Hermes (пути темплейтятся под эту машину)
    SkillInstall,
}

#[derive(Subcommand, Debug)]
pub enum SyncCommands {
    /// Статус: конфиг пирингов, watermark'ы, бандлы в outbox/inbox
    Status,
    /// Выгрузить бандл изменений для пира в data/sync/outbox/
    Export {
        /// Имя пира из peers.json (watermark ведётся на пира; дефолт: default)
        #[arg(short, long, default_value = "default")]
        peer: String,
    },
    /// Применить бандл(и) из файлов
    Import {
        /// Пути к файлам бандлов (.jsonl.gz)
        files: Vec<String>,
    },
    /// Применить все бандлы из data/sync/inbox/
    ApplyInbox,
    /// Экспорт + scp бандла на пир (method=ssh)
    Push {
        #[arg(short, long)]
        peer: String,
    },
    /// scp бандлов пира в inbox + применение (method=ssh)
    Pull {
        #[arg(short, long)]
        peer: String,
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
