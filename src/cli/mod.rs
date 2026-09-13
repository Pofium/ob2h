pub mod agent;
pub mod bench;
pub mod db;
pub mod doctor;

pub use agent::{AgentManager, AgentTarget};
pub use doctor::{Doctor, DoctorItem, DoctorStatus};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ob2h", author, version, about = "Локальное MCP-хранилище знаний для AI-агентов на Rust", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Запустить MCP stdio сервер (поведение по умолчанию)
    Serve,
    /// Диагностика окружения, баз данных, моделей и AI-агентов
    Doctor {
        /// Автоматически исправить отсутствующие конфигурации агентов и Git-хуков
        #[arg(short, long)]
        fix: bool,
    },
    /// Управление процессом дриминга памяти
    Dream {
        #[command(subcommand)]
        command: DreamCommands,
    },
    /// Создать бэкап (full|quick) или проверить существующий (backup verify <path>)
    Backup {
        /// Скоуп создания: full (вся БД) | quick (память+воркспейс, Ф24)
        #[arg(short, long, default_value = "full")]
        scope: String,
        /// Проверить существующий бэкап (каталог бэкапа или файл БД) вместо создания
        #[arg(long)]
        verify: Option<String>,
    },
    /// Служебные операции с БД
    Db {
        #[command(subcommand)]
        command: DbCommands,
    },
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
    /// Управление интеграциями с AI-агентами (Claude, Cursor, Windsurf, ZCode, Gemini, Qwen, OpenCode)
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// Управление проектами и AST-сканированием кодовой базы
    Project {
        #[command(subcommand)]
        command: ProjectCliCommands,
    },
    /// Регрессионный bench retrieval: golden set, recall@k, MRR, латентность (Фаза 21)
    Bench {
        /// Подкоманда (напр. history — тренд ночных прогонов, Ф30)
        #[command(subcommand)]
        command: Option<BenchCommands>,
        /// Режим: search (memory_search hybrid) | context (memory_context / build_context)
        #[arg(short, long, default_value = "search")]
        mode: String,
        /// Уровни k для recall@k, через запятую
        #[arg(long, default_value = "5,10")]
        k: String,
        /// Путь к golden-набору (по умолчанию data/bench/golden.jsonl)
        #[arg(long)]
        golden: Option<String>,
        /// Машиночитаемый JSON-вывод
        #[arg(long)]
        json: bool,
        /// Сохранить агрегаты как baseline в docs/bench_baseline.md
        #[arg(long)]
        save_baseline: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum BenchCommands {
    /// Тренд ночных bench-прогонов гейта дрима из data/bench/history.jsonl (Ф30)
    History {
        /// Сколько последних строк показать
        #[arg(long, default_value_t = 20)]
        last: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum DbCommands {
    /// Разовое int8-квантование эмбеддингов (f32 → v2, ~4× компактнее)
    QuantizeEmbeddings {
        /// Только показать, что будет сделано
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentCommands {
    /// Установить и настроить интеграцию для агента
    Install {
        /// Целевой агент (all|claude|cursor|windsurf|zcode|gemini|qwen|opencode|hermes)
        #[arg(short, long, default_value = "all")]
        agent: AgentTarget,
        /// Кастомный путь к проекту (для локальной конфигурации .cursor / .zcode)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Показать статус подключения агентов к OB2H
    Status,
}

#[derive(Subcommand, Debug)]
pub enum ProjectCliCommands {
    /// Зарегистрировать новый проект
    Init {
        #[arg(short, long)]
        id: String,
        #[arg(short, long)]
        name: String,
        #[arg(short, long)]
        path: String,
        #[arg(short, long)]
        description: Option<String>,
    },
    /// Сканировать кодовую базу проекта через AST
    Scan {
        #[arg(short, long)]
        id: String,
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Список зарегистрированных проектов
    List,
    /// Сгенерировать архитектурный дайджест проекта
    Report {
        #[arg(short, long)]
        id: String,
    },
    /// Установить Git-хуки для автоматического инкрементального сканирования
    HookInstall {
        /// Путь к репозиторию (по умолчанию текущая директория)
        #[arg(short, long)]
        path: Option<String>,
        /// Идентификатор проекта (если опущен, определяется автоматически)
        #[arg(short, long)]
        id: Option<String>,
    },
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
