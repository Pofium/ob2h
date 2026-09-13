pub mod agent;
pub mod bench;
pub mod bench_history;
pub mod db;
pub mod dedup;
pub mod doctor;

pub use agent::{AgentManager, AgentTarget};
use clap::{Parser, Subcommand};
pub use doctor::{Doctor, DoctorItem, DoctorStatus};

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
    /// Операции с памятью (дедуп почти-дублей, Ф31)
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
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
    /// Ф35.1: эксперимент sqlite-vec — vec0-индекс над graph_nodes (флаг OB2H_VEC0=1)
    Vec0 {
        #[command(subcommand)]
        command: Vec0Commands,
    },
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
    /// Ralph Knowledge Layer: циклы разработки (Фазы 26–28)
    Ralph {
        #[command(subcommand)]
        command: RalphCommands,
    },
    /// Регрессионный bench retrieval: golden set, recall@k, MRR, латентность (Фаза 21)
    Bench {
        /// Подкоманда (напр. history — тренд ночных прогонов, Ф30)
        #[command(subcommand)]
        command: Option<BenchCommands>,
        /// Режим: search (memory_search hybrid) | context (build_context) | latency (p50/p95 по memories и graph_nodes, Ф35.1)
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
pub enum RalphCommands {
    /// Перенести verified-findings проекта в долговременную память (FR-K12)
    FindingsToMemory {
        /// Идентификатор проекта
        #[arg(short, long)]
        project: String,
        /// Только показать, что будет перенесено
        #[arg(long)]
        dry_run: bool,
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
pub enum MemoryCommands {
    /// Отчёт о почти-дублях памяти (косинус ≥ 0.75; без LLM). Без --dry-run
    /// помечает пары meta.merge_candidate — слияние решает дрим (Ф31.5)
    Dedup {
        /// Только отчёт, маркеры не ставить
        #[arg(long)]
        dry_run: bool,
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
    /// Ф37: repo-map — карта «файл → символы» под token budget (Aider-паттерн)
    RepoMap {
        #[arg(short, long)]
        id: String,
        /// Бюджет в токенах (2k/4k/8k)
        #[arg(long, default_value_t = 4096)]
        tokens: usize,
        /// Фокус: файлы, совпадающие с query, получают PPR-сид
        #[arg(short, long)]
        query: Option<String>,
        /// Ф40.3: подмешать high-trust память проекта
        #[arg(long, default_value_t = false)]
        memory: bool,
    },
    /// Ф36.3: кандидаты в мёртвый код — символы с in-degree 0 (кроме entrypoints)
    DeadCode {
        #[arg(short, long)]
        id: String,
        /// Лимит вывода (дефолт: 50)
        #[arg(long, default_value = "50")]
        limit: usize,
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
    /// Выгрузить бандл изменений для пира в data/sync/outbox/ (Ф34.1: дефолт — дельта)
    Export {
        /// Имя пира из peers.json (watermark ведётся на пира; дефолт: default)
        #[arg(short, long, default_value = "default")]
        peer: String,
        /// Ф34.1: полный бандл (игнорируя курсор) — ежемесячно/по запросу
        #[arg(long)]
        full: bool,
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
        /// Ф34.1: полный бандл (игнорируя курсор)
        #[arg(long)]
        full: bool,
    },
    /// scp бандлов пира в inbox + применение (method=ssh)
    Pull {
        #[arg(short, long)]
        peer: String,
    },
    /// Ф34.4: сверка с пиром без переноса — counts, trust_avg, контрольные суммы
    Verify {
        #[arg(short, long)]
        peer: String,
    },
    /// Ф34.4: статистика этой стороны в JSON (вызывается удалённой стороной по ssh)
    LocalStats,
}

/// Ф35.1: команды эксперимента vec0.
#[derive(Subcommand, Debug)]
pub enum Vec0Commands {
    /// Построить/догнать индекс над graph_nodes
    Build {
        /// Полная перестройка индекса
        #[arg(long)]
        rebuild: bool,
    },
    /// Статистика индекса (узлов с эмбеддингом / проиндексировано)
    Stats,
    /// Recall@k vec0+рескоринг против полного перебора на golden-запросах
    Recall {
        /// Уровень k (критерий приёмки — recall@10 ≥ 0.99)
        #[arg(long, default_value = "10")]
        k: usize,
        /// Ограничить число golden-запросов (по умолчанию — все)
        #[arg(long)]
        limit: Option<usize>,
        /// Путь к golden-набору (по умолчанию data/bench/golden.jsonl)
        #[arg(long)]
        golden: Option<String>,
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
