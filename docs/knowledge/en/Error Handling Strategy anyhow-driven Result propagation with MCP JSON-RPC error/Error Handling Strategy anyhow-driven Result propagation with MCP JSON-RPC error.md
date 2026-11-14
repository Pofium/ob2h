---
kind: error_handling
name: 'Error Handling Strategy: anyhow-driven Result propagation with MCP JSON-RPC error encoding'
category: error_handling
scope:
    - '**'
source_files:
    - src/main.rs
    - src/mcp/protocol.rs
    - src/db/mod.rs
    - src/embedding/api.rs
    - src/lifecycle/lessons.rs
    - src/lifecycle/slots.rs
    - src/cli/bench.rs
    - src/backup/mod.rs
---

# Error Handling in OB2H

## Approach Overview

OB2H is a Rust binary that uses **`anyhow`** as its primary error type across the entire codebase. There is no custom `Error` enum, no `thiserror`-derived types, and no repository-wide sentinel-error hierarchy. Errors propagate as `anyhow::Result<T>` (or `std::result::Result<T, anyhow::Error>`) from deep library modules up to the CLI dispatch layer, where they are either printed to stderr or cause an exit with status 1.

The only domain-specific error struct is `JsonRpcError` in `src/mcp/protocol.rs`, which wraps errors into the MCP (JSON-RPC 2.0 stdio) wire format:

```rust
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}
```

This is the sole place where errors cross the MCP protocol boundary; internal Rust code never uses it.

## Key Files

- `src/main.rs` — entry point; `main()` returns `anyhow::Result<()>`; all subcommands return `anyhow::Result<()>` or `Result<(), anyhow::Error>`; errors bubble up to the top-level match and terminate the process via `eprintln!` + `std::process::exit(1)` in a few places (update check failures, update apply).
- `src/mcp/protocol.rs` — defines `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`, `McpToolCallResult` (with optional `is_error` flag). This is the MCP wire-level error contract.
- `src/db/mod.rs` — wraps `rusqlite` errors into `anyhow::Result` via `map_err(|e| anyhow::anyhow!("DB lock error: {e}"))` and provides `with_conn` / `with_tx` helpers that convert database errors into `anyhow`.
- `src/embedding/api.rs` — validates embedding provider URLs and converts HTTP/API failures into `anyhow::bail!` messages.
- `src/lifecycle/lessons.rs`, `src/lifecycle/slots.rs` — validation errors use `anyhow::bail!("[Error] ... [Подсказка: ...]")` with a consistent `[Error]` / `[Подсказка:]` prefix pattern for user-facing validation failures.
- `src/cli/bench.rs`, `src/backup/mod.rs`, `src/ingest/mod.rs` — additional sites of `anyhow::bail!` for argument validation and I/O failures.

## Architecture & Conventions

1. **Uniform `anyhow::Result` propagation.** Nearly every public function in `src/` returns `anyhow::Result<...>`. The dependency chain goes: low-level I/O → `anyhow::bail!` → higher layers → `main.rs` dispatch → print/exit.

2. **No custom error types.** No `enum XxxError`, no `#[derive(thiserror::Error)]`, no `impl std::error::Error` for application types. Errors are opaque strings produced by `anyhow::bail!` or wrapped library errors converted via `map_err`.

3. **Validation errors follow a human-readable template.** In lifecycle modules, validation failures consistently use the pattern:
   ```
   "[Error] <description> [Подсказка: <hint>]"
   ```
   Examples: lessons content length, slot label constraints, confidence range checks.

4. **MCP error encoding is separate from internal errors.** Internal functions return `anyhow::Result`; when an error needs to be sent over the MCP stdio channel, it is serialized as a `JsonRpcError` with a numeric `code`, a `message` string, and optional `data`. The `McpToolCallResult` also carries an optional `is_error: bool` field for tool-call responses.

5. **Process termination on unrecoverable errors.** In `main.rs`, certain failure paths explicitly call `std::process::exit(1)` after printing to stderr (e.g., update fetch failures at lines 375–377, 422–425, 446–449). Most other errors simply bubble up and let `main()`'s `anyhow::Result` cause a non-zero exit.

6. **Logging via tracing, not error reporting.** Errors are logged through `tracing::debug!` / `tracing::info!` rather than being embedded in error values. The `EnvFilter` is initialized early in `main()` and logs go both to a rolling file (`ob2h.log`) and to stderr depending on mode.

7. **`unwrap` / `expect` usage is limited and mostly in tests or trusted invariants.** Observed occurrences include test code (`db/mod.rs` tests), benchmark setup (`cli/bench.rs` temp files), and a few internal assumptions (e.g., `NonZeroUsize::new(...).expect("capacity > 0")` in `embedding/mod.rs`). No `panic!` calls were found in production code; the only `panic!`-adjacent references are comments about SQLite WAL recovery in `viewer/data.rs`.

8. **Background tasks swallow errors gracefully.** The update-check task spawned in `serve` mode catches `FetchError` and logs it at debug level rather than propagating it to the server loop.

## Conventions & Constraints

- **Convention:** Application-layer errors are surfaced via `anyhow::bail!(...)` with descriptive, user-oriented messages. Library-style wrappers around third-party crates (`rusqlite`, HTTP clients) convert their errors into `anyhow` using `map_err` or direct `bail!`.
- **Convention:** MCP-boundary errors are encoded as `JsonRpcError` structs defined in `src/mcp/protocol.rs`; this is the only place where error codes and structured error payloads are defined.
- **Convention:** Validation errors in lifecycle modules use the `[Error] ... [Подсказка: ...]` two-part message format, making them distinguishable from runtime failures.
- **Constraint:** There is no centralized error taxonomy or error-code registry — error classification is ad-hoc per module.
- **Constraint:** No `panic!` is used in production code paths observed in `src/`; recoverable failures are returned as `Result`s. Process exits are reserved for fatal startup/configuration errors.
- **Constraint:** The MCP protocol requires a `code: i64` field on `JsonRpcError`; callers must supply one when constructing these errors for transport.

## What Is Not Present

- No custom `Error` enum or `thiserror` usage.
- No middleware pipeline for error handling (the app is a CLI + MCP server, not a web framework).
- No `recover` / `catch_unwind` patterns.
- No error metrics or structured error counters beyond logging.