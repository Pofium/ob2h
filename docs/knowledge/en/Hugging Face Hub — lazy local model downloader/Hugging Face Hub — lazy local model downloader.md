---
kind: external_dependency
name: Hugging Face Hub — lazy local model downloader
slug: hugging-face-hub
category: external_dependency
category_hints:
    - vendor_identity
    - auth_protocol
scope:
    - '**'
source_files:
    - Cargo.toml
    - README.md
---

### Role
Hosts the default local mini-model `Qwen/Qwen3-0.6B` (~1.4 GB) downloaded lazily by `LocalMiniLLM` when no explicit LLM API key is configured.

### Integration shape
- Downloaded via the `hf-hub` crate; can be disabled with `OB2H_OFFLINE=1`.
- Model repo name is configurable via `OB2H_LOCAL_LLM_MODEL` (qwen3-compatible).