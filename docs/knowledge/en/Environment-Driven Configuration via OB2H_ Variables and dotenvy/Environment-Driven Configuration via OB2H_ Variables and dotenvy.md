---
kind: configuration_system
name: Environment-Driven Configuration via OB2H_* Variables and dotenvy
category: configuration_system
scope:
    - '**'
source_files:
    - src/config.rs
    - src/main.rs
    - Cargo.toml
---

## Approach

OB2H uses a **pure environment-variable configuration system** with no application-level config file format of its own. All runtime settings are loaded from `std::env` variables prefixed with `OB2H_`, optionally pre-populated by `.env` files through the `dotenvy` crate. There is no YAML/TOML/JSON application config loader for OB2H's own settings — those formats appear only in third-party integrations (Hermes `config.yaml`, `ob2h.json`).

The single source of truth is `src/config.rs`, which defines one central struct:

```rust
pub struct Settings {
    // storage, LLM, embeddings, dream/consolidation, retention,
    // watcher/autosync, auto-update, offline mode, secrets TTL,
    // logging, tool output limits, PPR graph weights, etc.
}
```

`Settings::from_env()` is the only constructor; `Default` delegates to it.

## Loading Order & Layering

1. **`.env` bootstrap**: `dotenvy::dotenv()` is called at the top of `from_env()`, loading `./.env` into process env vars.
2. **Process env vars**: Every field is read via `env::var("OB2H_<NAME>")` with an `unwrap_or_else` default.
3. **Special `OB2H_LLM_API_KEY` resolution** (`resolve_llm_api_key`):
   - If the value is empty → empty key, not explicit.
   - If the value exists as a real env var → use it directly (explicit).
   - If the value looks like an env-name pattern (`looks_like_env_name`: uppercase + digits + `_`, length ≥ 3) → search `<data_dir>/.env` then `./.env` for that name via `read_env_file_value` (a custom parser that ignores comments, strips quotes).
   - If still missing → log a warning and return the literal string (non-explicit), allowing harness-based substitution later.
4. **Derived state**: After all env reads, if `OB2H_OFFLINE=1|true`, `HF_HUB_OFFLINE` is set to `1` and update checks are disabled.
5. **Directory layout**: `ensure_dirs()` creates `data_dir`, `workspace/`, `workspace/memory/`, `workspace/daily/`, `backups/`, `logs/`, `tmp/` under `data_dir` (default `./data`).

## Environment Variable Reference (all `OB2H_` prefix)

| Category | Variables | Defaults |
|---|---|---|
| Storage | `OB2H_DATA_DIR` | `data` |
| LLM | `OB2H_LLM_BASE_URL`, `OB2H_LLM_API_KEY`, `OB2H_LLM_MODEL`, `OB2H_LLM_TIMEOUT`, `OB2H_LLM_MAX_RETRIES` | `https://api.deepseek.com/v1`, empty, `deepseek-v4-flash`, 120s, 3 |
| Embeddings | `OB2H_EMBED_PROVIDER`, `OB2H_EMBED_MODEL`, `OB2H_EMBED_BASE_URL`, `OB2H_EMBED_API_KEY` | `local`, `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2`, empty, empty |
| Context window | `OB2H_CONTEXT_WINDOW`, `OB2H_MAX_COMPLETION_TOKENS`, `OB2H_PREFETCH_MAX_CHARS`, `OB2H_HALF_LIFE_DAYS` | 65536, 8192, 8000, 90.0 |
| Dream/Autodream | `OB2H_AUTODREAM_ENABLED`, `OB2H_AUTODREAM_INTERVAL_MIN`, `OB2H_AUTODREAM_MIN_INTERVAL_H`, `OB2H_AUTODREAM_MIN_EVENTS`, `OB2H_DREAM_BATCH`, `OB2H_DREAM_EXTRACT_ENABLED`, `OB2H_DREAM_MEMORY_REVISION`, `OB2H_DREAM_BELIEF`, `OB2H_DREAM_RALPH` | true, 5, 4, 10, 20, true, true, false, true |
| Bench gate | `OB2H_BENCH_GATE`, `OB2H_BENCH_GATE_TIMEOUT_MS` | false, 3000 |
| Logging | `OB2H_LOG_LEVEL`, `OB2H_LOG_RETENTION_DAYS`, `OB2H_MAX_TOOL_OUTPUT_CHARS` | `INFO`, 90, 20000 |
| Graph PPR | `OB2H_PPR_WEIGHTS`, `OB2H_PPR_DAMPING`, `OB2H_GRAPH_REASON_MEMORY_MAX_NODES`, `OB2H_GRAPH_REASON_MEMORY_TIMEOUT_MS` | defaults from `crate::graph::pagerank`, 0.85, 500, 1000 |
| Retention/backups | `OB2H_RETENTION_DAYS`, `OB2H_BACKUP_KEEP_FULL`, `OB2H_BACKUP_KEEP_QUICK` | 30, 3, 14 |
| Watcher/sync | `OB2H_WATCHER_ENABLED`, `OB2H_WATCHER_DEBOUNCE_MS`, `OB2H_AUTOSYNC_ENABLED`, `OB2H_SYNC_INTERVAL_MINUTES` | true, 2500, true, 120 |
| Auto-update | `OB2H_UPDATE_ENABLED`, `OB2H_UPDATE_CHECK_ON_START`, `OB2H_UPDATE_INTERVAL_HOURS`, `OB2H_UPDATE_REPO`, `OB2H_UPDATE_CHANNEL` | true, true, 24, `Pofium/ob2h`, `stable` |
| Secrets/offline | `OB2H_SECRETS_TTL_MIN`, `OB2H_OFFLINE` | 30, false |

Boolean parsing accepts both `"0"`/`"false"` (disabled) and `"1"`/`"true"` (enabled); some flags are inverted (e.g. `dream_belief` requires exactly `"1"` or `"true"`).

## Integration Points

- **Binary entry point** (`src/main.rs`) calls `Settings::from_env()` first, then `settings.ensure_dirs()`, then wires tracing (file appender under `logs_dir()`, level from `EnvFilter::try_from_default_env()` falling back to `settings.log_level`).
- **Hermes integration**: `install_to_hermes()` writes an `mcp_servers.ob2h` block into Hermes' `config.yaml` with `OB2H_DATA_DIR`, `OB2H_LLM_BASE_URL`, `OB2H_LLM_API_KEY: "DEEPSEEK_API_KEY"`, `OB2H_LLM_MODEL`, `OB2H_EMBED_PROVIDER`, `OB2H_AUTODREAM_ENABLED`. The `plugin_install()` command also writes `HERMES_HOME/ob2h.json` with `binary` and `data_dir` paths.
- **Secrets vault** (`src/secrets/`) is separate from this config system — secrets are stored encrypted in the SQLite DB and accessed via IPC, with TTL controlled by `OB2H_SECRETS_TTL_MIN`.
- **Tests** in `src/config.rs` verify `looks_like_env_name`, literal passthrough, env-var priority over `.env`, fallback to `<data_dir>/.env`, and missing-name behavior.

## Conventions Observed

- Every configurable setting has a corresponding `OB2H_*` environment variable; there is no alternative config file format for OB2H's own runtime settings.
- Boolean fields consistently treat `"0"` and lowercase `"false"` as disabled; enabling typically requires `"1"` or `"true"`.
- Numeric fields parse via `v.parse().ok().unwrap_or(default)` — invalid values silently fall back to defaults.
- Path-like settings resolve relative to `data_dir` (not cwd) to avoid per-project directory drift when run from different working directories (e.g. Hermes plugin runs with cwd = hermes home).
- The `dotenvy` dependency is present but `.env` files are only consulted as a fallback for `OB2H_LLM_API_KEY` name indirection, not as a general config mechanism.
- Offline mode (`OB2H_OFFLINE=1|true`) is a hard override: it forces `HF_HUB_OFFLINE=1` and disables all update-check features.