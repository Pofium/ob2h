---
kind: external_dependency
name: GitHub — release artifacts and self-update source
slug: github
category: external_dependency
category_hints:
    - vendor_identity
    - client_constraint
scope:
    - '**'
source_files:
    - .github/workflows/release.yml
    - Cargo.toml
---

### Role
- Release pipeline: `.github/workflows/release.yml` builds Windows/Linux/macOS binaries on tag push (`vX.Y.Z`) and publishes them as a GitHub Release via `softprops/action-gh-release`.
- Self-update: `ob2h update check/apply` downloads the matching release asset from GitHub and replaces the running binary via `self-replace`.

### Integration shape
- Windows artifact built with `--features gui`; Linux/macOS headless only (see PLAN.md §6 decision).
- Verify exact download URL / asset naming against the current release workflow before changing the update client.