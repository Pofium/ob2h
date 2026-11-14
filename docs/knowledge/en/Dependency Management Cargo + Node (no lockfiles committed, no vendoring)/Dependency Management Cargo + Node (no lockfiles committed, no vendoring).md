---
kind: dependency_management
name: 'Dependency Management: Cargo + Node (no lockfiles committed, no vendoring)'
category: dependency_management
scope:
    - '**'
source_files:
    - Cargo.toml
    - plugin/dsh-ob2h/package.json
---

## Approach

OB2H is a Rust binary with one small Node.js plugin subpackage. Dependency management is split across two ecosystems:

- **Rust**: `Cargo.toml` at the repo root declares all dependencies; there is no workspace manifest — this is a single-crate project.
- **Node.js**: `plugin/dsh-ob2h/package.json` declares the DeepSeek Harness cordis plugin; it has no `dependencies`/`devDependencies` entries (the plugin only depends on Node stdlib and the ob2h binary over JSON-RPC stdio).

There is **no Python dependency manifest** (`pyproject.toml`, `requirements.txt`, `Pipfile`, `poetry.lock`) despite the AGENTS.md rule referencing `pyproject.toml`; the Python code under `plugin/ob2h/` and `plugin/tests/` appears to be pure stdlib / local imports.

## Key files

- `Cargo.toml` — sole source of truth for Rust dependencies (69 lines, ~40 direct deps).
- `plugin/dsh-ob2h/package.json` — minimal Node package manifest (no runtime deps, `engines.node >= 20`).
- `.gitignore` — checked below for lockfile policy.

## Lockfile policy

No lockfiles are committed to the repository. A grep across the tree finds zero instances of `Cargo.lock`, `package-lock.json`, `yarn.lock`, or `pnpm-lock.yaml`. The comment in `Cargo.toml` (lines 55–56) explicitly references `Cargo.lock` as an external artifact that already exists in the build tree but is not part of the repo:

```
# Ф70.1: проверка живости pid для lockfile viewer. Оба крейта уже в дереве
# как транзитивные (Cargo.lock) — новых скачиваний не добавляют.
```

This means versions are resolved at build time from the registry using the caret/pin ranges declared in `Cargo.toml`.

## Versioning conventions in `Cargo.toml`

- Most crates use **caret ranges** (e.g. `tokio = "1.43"`, `serde = "1.0"`, `clap = "4.5"`, `regex = "1.11"`), allowing patch-level updates without breaking changes.
- A few are pinned to exact minor versions where API stability matters more than auto-updates (e.g. `rusqlite = "0.33"`, `candle-core/nn/transformers = "0.9"`, `tokenizers = "0.21"`, `sqlite-vec = "0.1.9"`).
- Feature flags are used extensively to keep binaries lean: `reqwest` disables default features and enables only `json` + `rustls-tls`; `rustls` uses `aws-lc-rs` backend; `eframe` is gated behind an optional `gui` feature (line 63–66); platform-specific deps go under `[target.'cfg(windows)'.dependencies]` and `[target.'cfg(unix)'.dependencies]` (lines 57–61).
- The `gui` feature toggles `eframe` and `sys-locale`; headless builds fall back to `rpassword` for PIN entry (documented in the same comment block).

## Vendoring / private registries

- No `vendor/` directory exists.
- No `.cargo/config.toml` was found in the tree (checked via grep for `config.toml`).
- No `GOFLAGS`, `GOPRIVATE`, `go.mod`, or Go tooling is present — the project is not Go-based.
- No npm/yarn/pnpm configuration files exist beyond `plugin/dsh-ob2h/package.json`.
- Dependencies are fetched directly from crates.io and the public npm registry.

## CI / release

The GitHub Actions workflow `.github/workflows/release.yml` triggers on tags and publishes artifacts; it does not introduce custom dependency resolution steps (no `cargo vendor`, no private registry auth). The release process relies on standard `cargo publish` / `cargo build` behavior against the public registry.

## Conventions observed

- All third-party Rust dependencies are declared centrally in the root `Cargo.toml` under `[dependencies]` and `[dev-dependencies]`; there is no per-module `Cargo.toml`.
- Platform-scoped dependencies are isolated under `[target.'cfg(...)'.dependencies]` rather than conditional compilation inside code.
- Optional GUI functionality is exposed as a Cargo feature (`gui`) so CI can build headless releases without pulling GUI transitive deps.
- Node plugin has no runtime dependencies — it is a thin wrapper around the ob2h binary, keeping its own dependency surface at zero.
- Lockfiles are intentionally excluded from version control; reproducibility is expected to come from pinned ranges in manifests plus CI caching of the registry cache.

## Rules enforced by the codebase

- **Rule**: Rust dependencies must be declared in the root `Cargo.toml` — there is no other manifest file in the repo.
- **Rule**: Platform-specific dependencies must be placed under `[target.'cfg(...)'.dependencies]` — both `windows-sys` and `libc` follow this pattern.
- **Rule**: GUI-related crates (`eframe`, `sys-locale`) must be gated behind the `gui` feature — they are marked `optional = true` and re-exported only through the `gui` feature flag.
- **Rule**: Lockfiles (`Cargo.lock`, `package-lock.json`, etc.) are not committed — verified by absence in the tree and explicit reference to `Cargo.lock` as an external build artifact in `Cargo.toml` comments.