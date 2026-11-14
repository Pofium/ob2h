---
kind: logging_system
name: Tracing-based Structured Logging with File + Stderr Sinks
category: logging_system
scope:
    - '**'
source_files:
    - src/main.rs
    - Cargo.toml
---

## System Overview

OB2H uses the Rust `tracing` ecosystem for structured logging:
- `tracing` (0.1) — macro API (`tracing::info!`, `debug!`, `warn!`, `error!`, `trace!`) used throughout the codebase.
- `tracing-subscriber` (0.3, features `env-filter`, `fmt`) — subscriber that formats and routes log records.
- `tracing-appender` (0.2) — rolling file appender used to write logs to disk.

There is no custom logger abstraction; modules call `tracing::*` macros directly after importing them locally.

## Initialization & Sinks

All initialization lives in `src/main.rs` (lines 40–58):

1. A rolling file writer is created via `tracing_appender::rolling::never(settings.logs_dir(), "ob2h.log")` — one persistent file per process, never rotated by age/size.
2. The file writer is wrapped in `tracing_appender::non_blocking` to avoid blocking the event loop.
3. An `EnvFilter` is built from `OB2H_LOG_LEVEL` env var (via `try_from_default_env()`), falling back to `settings.log_level`.
4. Two subscriber configurations are selected based on whether the process runs in stdio MCP mode (`is_stdio`):
   - **stdio mode** (`serve` or no subcommand): only the non-blocking file sink.
   - **CLI mode**: both `stderr` and the file sink.

This means interactive CLI output goes to stderr while all structured logs always go to `<data_dir>/logs/ob2h.log`.

## Log Levels

The project uses the standard five tracing levels:
- `debug!` — update-check diagnostics, auto-dream skip reasons.
- `info!` — backup creation, nightly bench status, self-replace shutdown, background worker events.
- `warn!` — failed operations (update fetch, consolidation, dedup counter save, AST processing).
- `error!` — fatal failures (imported but not seen in the sampled snippets).
- `trace!` — imported in some modules but not observed in the sampled callsites.

Log level filtering is controlled at startup through `EnvFilter`; there is no runtime toggle.

## Conventions Observed

- Each module imports only the macros it needs (e.g. `use tracing::info;`, `use tracing::{info, warn};`) rather than a blanket `use tracing::*`. This is visible across `backup/mod.rs`, `dream/*`, `embedding/*`, `project/*`, `llm/*`, `secrets/ipc.rs`, etc.
- Messages are written in Russian where they describe user-visible state transitions (e.g. `"Проверка обновлений не удалась: {msg}"`, `"Процесс завершается для замены бинарника (self-replace в действии)."`).
- No structured fields (key-value pairs) are attached to log records in the sampled code — messages are plain interpolated strings. There is no use of `tracing::field!` or JSON enrichment beyond the default subscriber format.
- The viewer process bypasses tracing entirely: it exits before `init_app` / logging setup (see the comment on lines 22–24 of `main.rs`).
- Error propagation uses `anyhow::Result` and `?`; errors are surfaced to callers and logged at the boundary (e.g. MCP server warnings around failed consolidation).

## Constraints Enforced by Code

- Log output location is derived from `settings.logs_dir()` and always named `ob2h.log` — callers cannot override it.
- In stdio MCP mode, nothing is written to stderr by the tracing subscriber; stdout remains free for MCP protocol traffic.
- In non-stdio mode, every log record is duplicated to both stderr and the file sink.
- Log verbosity is gated exclusively by `EnvFilter` constructed from `OB2H_LOG_LEVEL` or `Settings.log_level`; there is no programmatic way to change it after initialization.

## Key Files

- `src/main.rs` — tracing subscriber bootstrap, EnvFilter, dual-sink selection.
- `Cargo.toml` — declares `tracing`, `tracing-subscriber` (with `env-filter`, `fmt`), `tracing-appender`.
- Module files using tracing: `src/backup/mod.rs`, `src/config.rs`, `src/consolidator/mod.rs`, `src/dream/{autodream,bench_gate,consolidate,mod}.rs`, `src/embedding/{fastembed_local,local_bert,mod}.rs`, `src/extractor/mod.rs`, `src/graph/service.rs`, `src/llm/{harness,local,openai}.rs`, `src/project/{hooks,scanjob,watcher}.rs`, `src/secrets/ipc.rs`, `src/cli/bench_history.rs`, `src/db/mod.rs`, `src/mcp/server.rs`, `src/project/ast.rs`.