This module is a flat collection of Markdown design documents plus an `issues/` subdirectory; there is no code or build artifact here.

- `ARCHITECTURE.md` is the canonical reference: it defines the two connection modes (Mode A native MemoryProvider plugin vs Mode 0 MCP stdio), enumerates the 36 MCP tools/resources/prompts, documents the SQLite schema (`memories`, `graph_nodes/edges`, `projects`, `project_files`, `sync_state`, …) with migration history M1–M7+, the file workspace layout under `data/workspace/`, env config (`OB2H_*`), reliability guarantees (WAL + busy_timeout + backups), and addendums tracking v1.3/v1.4/v1.5/v1.7 deltas.
- `SYNC.md` is the operational guide for the PC↔VPS peer sync protocol (bzip'd JSONL bundles, LWW merge, tombstones, field-level merge in v2, ssh/manual transport, systemd/Task Scheduler scheduling).
- `HERMES_INTEGRATION.md` / `DSH_INTEGRATION.md` document the host integrations.
- `PLAN_v0.8.md`, `PLAN_v1.7_bench.md` are the ADR source-of-truth referenced by `ARCHITECTURE.md` (§1 ADR-1…ADR-8, §6 ADR-9…ADR-13).
- `ADR-35.1-sqlite-vec.md` records the vector-search decision; `ANALYSIS_omnesbot_ast.md` and `REFERENCE_omnesbot.md` are porting references from OmnesBOT.
- `bench_baseline.md` captures benchmark baselines.
- `issues/DRAFT_ISSUES.md` holds pre-written GitHub issue templates (labels, title, body, reproduction steps) produced from AST analysis of OmnesBOT.

Dependency direction is one-way: these docs describe the Rust implementation in `src/` but do not depend on it at runtime; cross-references between docs use relative paths and section anchors rather than generated links.