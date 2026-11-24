# ob2h

[中文文档](./README_CN.md)

Gives a ZCode session long-term memory, a knowledge graph and an AST graph of the code it
works on. The plugin starts [ob2h](https://github.com/Pofium/ob2h) (OmnesBot to Harness), a
local MCP server written in Rust, and teaches the agent when to recall, when to save and how
to read a code base through it. Everything ob2h learns stays in one folder on the user's
machine.

## What the plugin provides

| Component | Name | Purpose |
| --- | --- | --- |
| MCP server | `ob2h` | The 57 tools of `ob2h serve`: memory, knowledge graph, AST code graph, lessons and slots, sessions and dreaming, development loops, secrets vault, housekeeping. |
| Skill | `ob2h` | When to recall and to save, how to use the code graph tools, which calls need a confirmation, and the pitfalls of the tools. Loaded when the user asks about memory, past decisions, the architecture of a project or the impact of a change. |
| Skill | `ob2h-setup` | Checks, installs or updates the ob2h executable, chooses the data directory and verifies that the MCP server starts. Installation and updates run only after the user agrees. |

## Requirements

- ob2h 2.0.1 or later on PATH, or its absolute path in the setting **ob2h executable**.
  Releases exist for Windows x86-64, Linux x86-64 (glibc and static musl) and macOS
  (Apple Silicon and Intel). The `ob2h-setup` skill installs it if it is missing.
- A data directory: an absolute folder path that the user enters in the setting
  **Data directory**. It is required, because ob2h has no default location for its store.

## Install

- **ZCode**: add the marketplace `Pofium/ob2h` (Plugin Marketplace → Add), then install
  **OB2H** under **Personal → ob2h-plugins**.
- **Claude Code** 2.1.275 or later: `/plugin install ob2h --marketplace Pofium/ob2h`.

## Getting started

1. Enable the plugin.
2. Open the settings of the plugin and enter **Data directory**, for example `~/.ob2h`.
   To keep using the memory of an existing ob2h installation, enter its `data` folder.
3. If ob2h is not installed, ask in a session: `Set up ob2h.` The agent runs the
   `ob2h-setup` skill and installs ob2h after asking for consent.
4. Start a new session, so that the MCP server starts, and ask:
   `What do you remember about me and my projects?` The agent calls `memory_context` and
   answers from the store. On a new store the answer is that nothing is saved yet.
5. Tell it something to keep: `Remember that I prefer tabs and Rust edition 2021.` The
   agent calls `memory_save`. The same question as in step 4 now returns the fact.

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `ob2h executable` (`binary`) | `ob2h` | Command or absolute path of the ob2h executable. The default uses the copy on PATH. |
| `Data directory` (`data_dir`) | none, required | Absolute path of the folder with the memory database, memory files, logs and backups (`OB2H_DATA_DIR`). It is created when the server first starts and is not removed when the plugin is uninstalled. |

Both settings are passed to the MCP server declared in `.mcp.json`. The plugin has no hooks.

## Security and side effects

- **Commands run.** The plugin starts one process, `<ob2h executable> serve`, and runs no
  other command. The `ob2h-setup` skill downloads and unpacks a release only after the user
  agrees. It names the commands that change other tools (`ob2h agent install`,
  `ob2h doctor --fix`, `ob2h update apply`) instead of running them.
- **Network access.** The plugin opens no connection itself. ob2h does:
  - **Model API.** Knowledge extraction, graph reasoning, session recaps and dreaming send
    text to an OpenAI-compatible API. ob2h takes the endpoint from the `OB2H_LLM_*`
    environment variables. When no key is set, it reads the model configuration of the host
    it is connected to (ZCode, Codex, Hermes) and uses that model, or falls back to a local
    model (Qwen3-0.6B, about 1.4 GB) that is downloaded from Hugging Face on first use.
    Set `OB2H_LLM_HARNESS_DETECT=0` to stop the reuse of the host model.
  - **Embeddings.** A local embedding model
    (`sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2`) is downloaded from
    Hugging Face on first use and cached. `OB2H_EMBED_PROVIDER=api` switches to an embedding
    API.
  - **Update check.** When the server starts, ob2h asks `api.github.com` at most once every
    24 hours whether a newer release exists. It only reports the result.
    `OB2H_UPDATE_ENABLED=0` disables the check.
  - **Sync between computers.** Only when the user configured peers in
    `<data directory>/sync/peers.json`: ob2h exchanges memory with those computers over SSH.
  - `OB2H_OFFLINE=1` forbids the model download and the update check.
- **Files written.** The plugin writes no files. ob2h writes only inside the **Data
  directory**: the SQLite database, `workspace/` (the memory files, with their own git
  repository), `backups/`, `logs/`, `run/` and `tmp/`. Downloaded models are kept in the
  Hugging Face cache of the user. When a session starts, ob2h adds the open project to its
  database as a record with the project path. It writes nothing into the project.
- **Safe defaults.** The secrets vault stays locked until the user enters a PIN in the local
  window or terminal. The PIN never passes through MCP or the conversation. Secrets are
  excluded from memory, graph, dreams and sync bundles. `knowledge_reconcile` only previews
    unless `apply` is true. Backups are rotated (3 full, 14 quick).
- **Untrusted input.** Memories, graph nodes, lessons, session logs and extracted documents
  are text that can come from third parties. The `ob2h` skill tells the agent not to follow
  instructions found there.
- **Data.** All data stays in the **Data directory** on this machine. It leaves the machine
  only in the API calls listed above.

## Troubleshooting

| Symptom | Action |
| --- | --- |
| The ob2h tools do not appear in a session | The executable was not found or **Data directory** is empty. Check both settings and the status of the MCP server in the settings of the host, ask the agent to run the `ob2h-setup` skill, then start a new session. |
| A search reports `warming_up_embeddings` | The embedding model is loading; on first use it is downloaded first. Retry after a few seconds. |
| `project_scan` answers that the scan is still running | Normal for large repositories. Poll `project_scan_status`. |
| A code graph search returns nothing | Pass the project `id` explicitly. Check `last_scanned_at` in `project_scan_status` and run `project_scan` when the project was never scanned. |
| `viewer_open` answers `gui_unavailable` | The build has no GUI or the machine is headless. Run `ob2h viewer` in a desktop session, or `ob2h stats`. |
| `secrets_get` reports that the vault is locked | The user unlocks it with the PIN in the local window, or runs `ob2h secrets unlock` in a terminal. The agent never asks for the PIN. |
| CLI commands do not see the memories of the plugin | Run `ob2h` commands with `OB2H_DATA_DIR` set to the **Data directory**. |

## Links

- Documentation: <https://github.com/Pofium/ob2h#readme>
- Source code: <https://github.com/Pofium/ob2h>
- Releases: <https://github.com/Pofium/ob2h/releases>
- Issues: <https://github.com/Pofium/ob2h/issues>

## License

The files of this plugin are licensed under the MIT License ([LICENSE](./LICENSE)).
ob2h itself is distributed under the MIT License; see
<https://github.com/Pofium/ob2h/blob/main/LICENSE>. The plugin contains no third-party code
or assets. The models that ob2h downloads on demand are distributed by their authors under
the licenses shown on their Hugging Face model pages.
