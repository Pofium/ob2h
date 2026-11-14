---
kind: business_term
name: Business Glossary
category: business_term
scope:
    - '**'
---

### Dreaming
- Definition：The nightly background process that analyzes the day's conversation history, removes stale facts, updates memory files (SOUL.md, USER.md, MEMORY.md), and persists versions in Git; includes a night-time bench gate that rolls back any dream that degrades recall/MRR.
- Aliases：дриминг、dream

### Ralph Knowledge Layer
- Definition：A knowledge layer recording development-cycle state (test verdicts, AST deltas, reuse candidates, debt ledger) to drive autonomous agentic development loops; exposed via MCP tools `ralph_start/iteration/verdict/context/report` and CLI `ob2h ralph findings-to-memory|debt`.
- Aliases：Ralph、ralph-loop

### Lessons
- Definition：Procedural experience records (what to do / avoid / when applicable) with confidence and fingerprint-based deduplication; reinforced lessons strengthen existing entries rather than creating duplicates, and a weekly decay sweep runs during dreaming without auto-deletion.
- Aliases：уроки、lessons

### Slots
- Definition：Editable agent workspace context (notes, TODOs, rules) stored per-project/global; append-on-overflow with a 1..20000-character budget, surfaced in `<agent_slots>` after memory blocks in prefetch with a default 1500-character budget (`OB2H_SLOTS_MAX_CHARS`).
- Aliases：слоты、slots

### Agent slots
- Definition：The `<agent_slots>` block injected into agent context after the memory block; contains pinned + non-empty slots in alphabetical order, budgeted by `OB2H_SLOTS_MAX_CHARS` (default 1500 chars) and returned even when memory is empty.
- Aliases：<agent_slots>、agent_slots

### Viewer
- Definition：Native read-only window (`ob2h viewer` or MCP `viewer_open`) providing tabs for Memory (FTS search), Graph (force-layout pan/zoom), Sessions replay, Lessons·Slots, Statistics, and Secrets; single-instance lockfile, RU/EN UI, dark/light theme, auto-refresh every 2 s (+ F5).
- Aliases：окно просмотра、viewer

### Secrets Vault
- Definition：Local encrypted store for passwords/tokens using XChaCha20-Poly1305 with an Argon2id master key held only in RAM (zeroized on TTL/lock/close); unlocked via a 4-digit PIN entered in the GUI (or terminal fallback), never sent over MCP/LLM/chat; fully isolated from memory/graph/dream/sync bundles.
- Aliases：секреты、secrets、secrets vault

### Sync bundle
- Definition：Incremental gzip delta bundle exchanged between machines over SSH containing memories, trust, memory_links, deletions, lessons, and slots; LWW merge on `updated_at` with conflict journal (`conflicts.jsonl`) and `meta.conflict_versions`; live DB is never transferred.
- Aliases：бандл синка、sync bundle、delta bundle

### DSH integration
- Definition：Two-layer DeepSeek Harness integration: MCP server via `@deepseek-ai/dsh-mcp-client` exposing 56 `mcp__ob2h__*` tools, plus the cordis plugin `plugin/dsh-ob2h` that injects `<agent_memory>` and auto-writes completed turns — mirroring Hermes' MemoryProvider behavior.
- Aliases：dsh、DeepSeek Harness、cordis plugin

### MemoryProvider-плагин
- Definition：Recommended Hermes integration mode where `ob2h plugin install` copies the provider into `$HERMES_HOME/plugins/ob2h/` and writes `ob2h.json`; automatically writes each turn, injects `<agent_memory>` before turns, exposes graph/dream tools, and survives ob2h restarts with backoff.
- Aliases：режим A/B、plugin mode

### Prefetch
- Definition：Relevant memories injected into the prompt before each turn; budget controlled by `OB2H_PREFETCH_MAX_CHARS` (default 8000, cut at record boundaries with a truncated marker); includes `[related]`/`[ppr]` neighbor blocks and optional `[conflicts]` and `<blast_radius>` warning blocks.
- Aliases：prefetch、предвыборка
