//! Конфигурация OB2H (переменные окружения с префиксом OB2H_).

use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Settings {
    // --- Хранилища ---
    pub data_dir: PathBuf,

    // --- LLM (dream / extract / reason / consolidate) ---
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_timeout_secs: f64,
    pub llm_max_retries: u32,

    // --- Эмбеддинги ---
    pub embed_provider: String, // "local" | "api"
    pub embed_model: String,
    pub embed_base_url: String,
    pub embed_api_key: String,

    // --- Консолидация / контекст ---
    pub context_window: usize,
    pub max_completion_tokens: usize,
    /// Бюджет символов блока <agent_memory> (Фаза 22): резать по record-границам в Rust.
    pub prefetch_max_chars: usize,
    /// Полураспад recency в днях для скоринга контекста (Фаза 22).
    pub recency_half_life_days: f64,

    // --- Дриминг ---
    pub autodream_enabled: bool,
    pub autodream_interval_min: u64,
    pub autodream_min_interval_h: i64,
    pub autodream_min_events: usize,
    pub dream_batch: usize,
    pub dream_extract_enabled: bool,
    pub dream_memory_revision: bool,

    // --- Ф30: ночной bench-гейт дрима ---
    /// Гейт выключен по умолчанию — до набора статистики (Ф30.3).
    pub bench_gate_enabled: bool,
    /// Бюджет quick-прогона bench; таймаут ≠ rollback (Ф30.2).
    pub bench_gate_timeout_ms: u64,

    // --- Ф33: Personalized PageRank по памяти ---
    /// Веса рёбер по типу (JSON `{"kind": weight}`); неизвестный kind → 0.5 (33.1).
    pub ppr_weights: crate::graph::pagerank::PprWeights,
    /// Damping PPR (клампится в 0.5–0.85, старт 0.85 — подбирается на bench).
    pub ppr_damping: f64,
    /// Предел узлов memory-подграфа в `graph_reason(scope=memory)` (33.2).
    pub graph_reason_memory_max_nodes: usize,
    /// Бюджет времени PPR-подграфа в `graph_reason(scope=memory)`, мс (33.2).
    pub graph_reason_memory_timeout_ms: u64,

    // --- Ретеншн ---
    pub retention_days: i64,
    /// Ротация полных бэкапов (Ф24).
    pub backup_keep_full: usize,
    /// Ротация быстрых бэкапов памяти (Ф24).
    pub backup_keep_quick: usize,

    // --- Реактивная автоматизация (Фаза 18) ---
    pub watcher_enabled: bool,
    pub watcher_debounce_ms: u64,
    pub autosync_enabled: bool,
    pub autosync_interval_minutes: u64,

    // --- Служебное ---
    pub log_level: String,
    pub max_tool_output_chars: usize,
}

impl Settings {
    pub fn from_env() -> Self {
        let _ = dotenvy::dotenv();

        let data_dir = env::var("OB2H_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data"));

        let llm_base_url = env::var("OB2H_LLM_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/v1".to_string());
        // OB2H_LLM_API_KEY может содержать ИМЯ переменной окружения с ключом
        // (конвенция из README, так настраивает install.bat/Hermes),
        // либо сам ключ. Разворачиваем индирекцию, с фолбэком на литерал.
        let key_ref = env::var("OB2H_LLM_API_KEY").unwrap_or_default();
        let llm_api_key = if key_ref.is_empty() {
            String::new()
        } else {
            env::var(&key_ref).unwrap_or_else(|_| key_ref.clone())
        };
        let llm_model = env::var("OB2H_LLM_MODEL")
            .unwrap_or_else(|_| "deepseek-v4-flash".to_string());
        let llm_timeout_secs = env::var("OB2H_LLM_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120.0);
        let llm_max_retries = env::var("OB2H_LLM_MAX_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);

        let embed_provider = env::var("OB2H_EMBED_PROVIDER")
            .unwrap_or_else(|_| "local".to_string());
        let embed_model = env::var("OB2H_EMBED_MODEL")
            .unwrap_or_else(|_| "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2".to_string());
        let embed_base_url = env::var("OB2H_EMBED_BASE_URL").unwrap_or_default();
        let embed_api_key = env::var("OB2H_EMBED_API_KEY").unwrap_or_default();

        let context_window = env::var("OB2H_CONTEXT_WINDOW")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(65536);
        let max_completion_tokens = env::var("OB2H_MAX_COMPLETION_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8192);
        let prefetch_max_chars = env::var("OB2H_PREFETCH_MAX_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8000);
        let recency_half_life_days = env::var("OB2H_HALF_LIFE_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90.0);

        let autodream_enabled = env::var("OB2H_AUTODREAM_ENABLED")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);
        let autodream_interval_min = env::var("OB2H_AUTODREAM_INTERVAL_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        let autodream_min_interval_h = env::var("OB2H_AUTODREAM_MIN_INTERVAL_H")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let autodream_min_events = env::var("OB2H_AUTODREAM_MIN_EVENTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        let dream_batch = env::var("OB2H_DREAM_BATCH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let dream_extract_enabled = env::var("OB2H_DREAM_EXTRACT_ENABLED")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);
        // Ф23.2: dream-ревизия памяти (trust-вердикты); дельты фиксированы, без автоудалений.
        let dream_memory_revision = env::var("OB2H_DREAM_MEMORY_REVISION")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);

        // Ф30: ночной bench-гейт дрима (дефолт off до набора статистики).
        let bench_gate_enabled = env::var("OB2H_BENCH_GATE")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);
        let bench_gate_timeout_ms = env::var("OB2H_BENCH_GATE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000);

        // Ф33: Personalized PageRank по памяти (веса рёбер по типу + damping).
        let ppr_weights = crate::graph::pagerank::parse_ppr_weights(
            &env::var("OB2H_PPR_WEIGHTS")
                .unwrap_or_else(|_| crate::graph::pagerank::DEFAULT_PPR_WEIGHTS.to_string()),
        );
        let ppr_damping = env::var("OB2H_PPR_DAMPING")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.85);
        let graph_reason_memory_max_nodes = env::var("OB2H_GRAPH_REASON_MEMORY_MAX_NODES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500);
        let graph_reason_memory_timeout_ms = env::var("OB2H_GRAPH_REASON_MEMORY_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);

        let retention_days = env::var("OB2H_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let backup_keep_full = env::var("OB2H_BACKUP_KEEP_FULL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let backup_keep_quick = env::var("OB2H_BACKUP_KEEP_QUICK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(14);

        let log_level = env::var("OB2H_LOG_LEVEL")
            .unwrap_or_else(|_| "INFO".to_string());
        let max_tool_output_chars = env::var("OB2H_MAX_TOOL_OUTPUT_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20000);
        let watcher_enabled = env::var("OB2H_WATCHER_ENABLED")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);
        let watcher_debounce_ms = env::var("OB2H_WATCHER_DEBOUNCE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2500);
        let autosync_enabled = env::var("OB2H_AUTOSYNC_ENABLED")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);
        let autosync_interval_minutes = env::var("OB2H_SYNC_INTERVAL_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120);

        Self {
            data_dir,
            llm_base_url,
            llm_api_key,
            llm_model,
            llm_timeout_secs,
            llm_max_retries,
            embed_provider,
            embed_model,
            embed_base_url,
            embed_api_key,
            context_window,
            max_completion_tokens,
            prefetch_max_chars,
            recency_half_life_days,
            autodream_enabled,
            autodream_interval_min,
            autodream_min_interval_h,
            autodream_min_events,
            dream_batch,
            dream_extract_enabled,
            dream_memory_revision,
            bench_gate_enabled,
            bench_gate_timeout_ms,
            ppr_weights,
            ppr_damping,
            graph_reason_memory_max_nodes,
            graph_reason_memory_timeout_ms,
            retention_days,
            backup_keep_full,
            backup_keep_quick,
            watcher_enabled,
            watcher_debounce_ms,
            autosync_enabled,
            autosync_interval_minutes,
            log_level,
            max_tool_output_chars,
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("ob2h.db")
    }

    pub fn workspace_dir(&self) -> PathBuf {
        self.data_dir.join("workspace")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }

    pub fn logs_dir(&self) -> PathBuf {
        // Логи под data_dir (README: OB2H_DATA_DIR — «папка БД, файлов памяти и логов»),
        // а не в относительный cwd/logs — иначе файл разъезжается по рабочим каталогам.
        self.data_dir.join("logs")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        let dirs = [
            &self.data_dir,
            &self.workspace_dir(),
            &self.workspace_dir().join("memory"),
            &self.workspace_dir().join("daily"),
            &self.backups_dir(),
            &self.logs_dir(),
        ];
        for d in dirs {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_env()
    }
}
