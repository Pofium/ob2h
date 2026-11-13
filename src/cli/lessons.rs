//! CLI: `ob2h lessons list|decay` (v1.9.9, Фаза 64).

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum LessonsCommands {
    /// Список уроков (по confidence desc, tombstones скрыты)
    List {
        /// Фильтр по проекту
        #[arg(short, long)]
        project: Option<String>,
        /// Фильтр по источнику: manual | consolidation | recap | dream
        #[arg(long)]
        source: Option<String>,
        /// Минимальная уверенность
        #[arg(long, default_value_t = 0.0)]
        min_confidence: f64,
        /// Максимум строк
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Недельный decay-sweep уроков (без удалений — ADR-11; кандидаты в отчёте)
    Decay,
}
