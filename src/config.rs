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

    // --- Дриминг ---
    pub autodream_enabled: bool,
    pub autodream_interval_min: u64,
    pub autodream_min_interval_h: i64,
    pub autodream_min_events: usize,
    pub dream_batch: usize,
    pub dream_extract_enabled: bool,

    // --- Ретеншн ---
    pub retention_days: i64,

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

        let retention_days = env::var("OB2H_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

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
            autodream_enabled,
            autodream_interval_min,
            autodream_min_interval_h,
            autodream_min_events,
            dream_batch,
            dream_extract_enabled,
            retention_days,
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
