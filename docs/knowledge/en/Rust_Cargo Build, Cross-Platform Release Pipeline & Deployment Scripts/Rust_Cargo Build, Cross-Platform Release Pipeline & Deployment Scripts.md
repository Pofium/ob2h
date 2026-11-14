---
kind: build_system
name: Rust/Cargo Build, Cross-Platform Release Pipeline & Deployment Scripts
category: build_system
scope:
    - '**'
source_files:
    - Cargo.toml
    - .github/workflows/release.yml
    - scripts/install_hermes.ps1
    - scripts/uninstall_hermes.ps1
    - scripts/pc/update_vps_ob2h.sh
    - scripts/vps/ob2h-sync.service
    - scripts/vps/ob2h-sync.timer
---

# Build System for OB2H

## Approach
OB2H is a single-crate Rust project built with **Cargo** and released via a **GitHub Actions** workflow. There are no Makefiles, Dockerfiles, or Python packaging tooling — the build surface is Cargo.toml plus a small set of deployment scripts.

## Key Files
- `Cargo.toml` — package metadata, dependencies, target-specific deps, and the optional `gui` feature.
- `.github/workflows/release.yml` — cross-platform release pipeline (Windows, Linux, macOS x86_64/aarch64).
- `scripts/install_hermes.ps1`, `scripts/uninstall_hermes.ps1` — Windows PowerShell installers that build the binary and patch a Hermes `config.yaml` to register the `ob2h serve` MCP server.
- `scripts/pc/update_vps_ob2h.sh` — one-shot script run from a Windows PC to push a freshly built binary onto a remote VPS via a Python bridge.
- `scripts/vps/ob2h-sync.service`, `scripts/vps/ob2h-sync.timer` — systemd unit + timer that runs `ob2h sync apply-inbox && ob2h sync export --peer pc` daily at 04:30.
- `scripts/pc/sync-task.ps1`, `scripts/pc/peers.example.json` — Windows-side sync scheduling helpers.

## Architecture & Conventions

### Versioning
- The version lives in one place: `[package].version = "1.9.9"` in `Cargo.toml`.
- Releases are triggered by pushing tags matching `v[0-9]+.[0-9]+.[0-9]+`; the workflow strips the leading `v` (`GITHUB_REF_NAME#v`) to derive the artifact name.
- The workflow also supports manual dispatch with an explicit `version` input when not triggered by a tag.

### Target Matrix
The CI builds four targets:
| OS | Target | Archive | Features |
|---|---|---|---|
| Windows | `x86_64-pc-windows-msvc` | `.zip` | `--features gui` |
| Linux | `x86_64-unknown-linux-gnu` | `.tar.gz` | none |
| macOS | `aarch64-apple-darwin` | `.tar.gz` | none |
| macOS | `x86_64-apple-darwin` | `.tar.gz` | none |

Only the Windows artifact ships with the `gui` feature enabled; Linux/macOS builds are headless (see comment referencing PLAN.md §6). The GUI feature gates `eframe` and `sys-locale` as optional dependencies.

### Artifact Assembly
Each job stages a directory `target/stage/<BIN_NAME>-<VERSION>` containing:
- the compiled binary (`ob2h.exe` on Windows, `ob2h` elsewhere),
- the `plugin/` and `skills/` directories,
- `README.md`, `CHANGELOG.md`, `AGENTS.md`.

Artifacts are then compressed per-OS (`.zip` on Windows via `Compress-Archive`, `.tar.gz` elsewhere via `tar -czf`) and a SHA256 checksum file is generated alongside each archive. Artifacts are uploaded under the matrix `build` name and later merged into a GitHub Release via `softprops/action-gh-release`.

### Dependency Management
- All dependencies are pinned by exact minor/patch versions in `Cargo.toml` (e.g. `tokio = "1.43"`, `rusqlite = "0.33"`).
- Platform-specific dependencies use Cargo's `[target.'cfg(windows)'.dependencies]` and `[target.'cfg(unix)'.dependencies]` sections (`windows-sys` on Windows, `libc` on Unix).
- The `gui` feature is declared as an optional feature that pulls in `eframe` and `sys-locale` only when requested.

### Local Development / Installation
- `scripts/install_hermes.ps1` expects the binary at `target\release\ob2h.exe`; if missing it runs `cargo build --release` before proceeding.
- It backs up the existing Hermes config to `<path>.bak`, removes any prior `ob2h:` block, then injects a new `mcp_servers.ob2h` entry pointing at the local binary with environment variables `OB2H_DATA_DIR`, `OB2H_LLM_BASE_URL`, `OB2H_LLM_API_KEY`, `OB2H_LLM_MODEL`, `OB2H_EMBED_PROVIDER=local`, and `OB2H_AUTODREAM_ENABLED=true`.
- `scripts/uninstall_hermes.ps1` reverses this operation.
- `scripts/pc/update_vps_ob2h.sh` stops the running `ob2h serve` process on the VPS, installs the locally built binary over `/usr/local/bin/ob2h`, and verifies the installed version.
- On the VPS, `ob2h-sync.service` (oneshot) and `ob2h-sync.timer` (daily at 04:30, persistent) orchestrate the sync flow; the service sets `OB2H_DATA_DIR=/root/ob2h_data`.

## Conventions & Constraints
- **Single source of truth for version**: `Cargo.toml`'s `[package].version` is the canonical version; the release workflow derives the tag/version from git tags rather than a separate manifest.
- **Cross-compilation without cross-toolchains**: the workflow installs the specific Rust target via `dtolnay/rust-toolchain@stable` with `targets: ${{ matrix.target }}` instead of using `cross` or `cargo-zigbuild`.
- **Feature-gated GUI**: the `gui` feature is the only optional feature; all non-Windows builds omit it, keeping CI artifacts headless.
- **Release trigger discipline**: releases are gated on tags matching `v[0-9]+.[0-9]+.[0-9]+`; manual dispatch requires an explicit semver string.
- **Artifact naming convention**: `<BIN_NAME>-<VERSION>-<matrix.build>.<ext>` where `BIN_NAME=ob2h`, `ext` is `.zip` on Windows and `.tar.gz` elsewhere.
- **Checksum requirement**: every published archive has a sibling `.sha256` file produced by `sha256sum`.
- **Hermes integration contract**: the PowerShell installer writes a YAML block whose indentation starts with two spaces (`ob2h:`) and nested keys with four spaces; uninstaller matches both YAML-style (`ob2h:`) and JSON-style (`"ob2h":`) prefixes to be tolerant of different config formats.