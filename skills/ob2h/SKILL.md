---
name: ob2h
description: Long-term memory, knowledge graph and dreaming backend of this Hermes agent. Use for any recall about the user, past conversations, projects, preferences, and for saving new important facts. Also use for developing/maintaining the ob2h project itself.
---

# OB2H — долговременная память этого агента (Rust, MCP + MemoryProvider)

## Поведение: память включена ВСЕГДА
- Каждый ход диалога попадает в ob2h автоматически: с v0.8 — MemoryProvider-плагин
  (`sync_turn`/`session_ingest`), без плагина (v0.7.1) — вызывай `session_log`
  после содержательных ответов сам, не дожидаясь просьбы.
- Вопросы о пользователе, его проектах, прошлых обсуждениях, предпочтениях → сначала
  recall (`memory_search` / блок `<agent_memory>`), потом ответ.
- Устные «запомни…» → `memory_save` с importance 0.8+; устойчивые факты — с `key`.
- Документы/длинные тексты → `knowledge_extract`; вопросы о связях «кто/что/чем связано» →
  `graph_reason`. Ночью/по гейтам авто-дрим обновляет MEMORY/SOUL/USER.md и граф.

## Режимы подключения (не путать при диагностике)
| Режим | Как выглядит | Что важно |
|---|---|---|
| **A: плагин (v0.8, целевой)** | `memory.provider: ob2h` в config.yaml, плагин в `$HERMES_HOME/plugins/ob2h/` | Один процесс `ob2h.exe serve` (спавнит плагин); `session_log`/`session_ingest` вызываются САМИ; индикатор 🧠 = prefetch сработал; `mcp_servers.ob2h` следует удалить |
| **B: переходный** | плагин И `mcp_servers.ob2h` одновременно | Работает (WAL+busy_timeout+dream-lock), но два процесса — лучше уйти в A |
| **0: MCP-only (v0.7.1, фолбэк)** | только `mcp_servers.ob2h` в config.yaml | Всё как раньше: захват только через явные вызовы инструментов |

## Что это
- Один Rust-бинарник ~15 МБ: MCP-сервер stdio + SQLite WAL + FTS5-trigram (русский)
  + локальные Candle-эмбеддинги `paraphrase-multilingual-MiniLM-L12-v2` (384d,
  кэш `~/.cache/huggingface/hub/`); LLM для экстракции/дрима — DeepSeek.
- Проект: `C:\Projects\omnesbot_for_hermes` (бинарник `target/release/ob2h.exe`,
  БД `data/ob2h.db`, workspace `data/workspace/`: SOUL.md/USER.md/memory/MEMORY.md +
  daily/*.jsonl, файлы создаются лениво — `workspace_read` отсутствующего = `""`).
- Инструменты: memory_save/search/update/forget/context, workspace_read/write,
  session_log, session_ingest (v0.8: bulk-транскрипта), knowledge_extract,
  graph_search/reason/stats, dream_run/status/log/restore, omnes_stats/backup.
- AutoDreamWorker: гейты ≥4ч и ≥10 событий, lock, git-история правок, бэкапы
  VACUUM INTO с ротацией 14. CLI: `ob2h stats | dream run/status/log/restore <sha> |
  backup | install/uninstall | plugin install/uninstall/status | skill install |
  sync status/export/import/push/pull` (plugin/skill/sync — с v0.8).

## Синхронизация двух машин (v0.8, PC ↔ VPS)
- Обмен — gzip-бандлы JSONL поверх SSH (`data/sync/peers.toml`: origin=pc|vps,
  приоритеты, пути). LWW по updated_at, удаления — tombstones. Идемпотентно.
- Живую `data/ob2h.db` НИКОГДА не синкать файловой синхронизацией (WAL = порча),
  только папку бандлов. Диагностика обмена: `ob2h sync status`.

## Критичные питфоллы
1. **OB2H_LLM_API_KEY = ИМЯ env-переменной** с ключом (напр. `DEEPSEEK_API_KEY`), не сам
   ключ (развязка в коде, коммит 82d7e3f; фолбэк на литерал). Симптом обратного:
   401 `Your api key: ****_KEY is invalid`.
2. **«chunks записались, entities=0»** в knowledge_extract — почти всегда упал LLM-вызов
   (см. п.1). Смотреть лог, не чинить экстрактор.
3. **Логи**: `data/logs/ob2h.log` (cwd сервера от Hermes = домашняя папка — до фикса
   82d7e3f логи могут лежать в `~/logs`, искать оба места).
4. **Пересборка**: `target/release/ob2h.exe` блокируется, пока процесс жив (Hermes/плагин
   держит). `cargo build --release` падает `os error 5` → сначала
   `Get-Process ob2h | Stop-Process -Force` (убьёт MCP в текущей сессии Hermes —
   спросить пользователя).
5. **ob2h ТОЛЬКО для этого Hermes**: не добавлять другим агентам, не оборачивать в
   mcp-compressor (пользователь запретил явно).
6. Живой сервер держит БД; read-only проверки через sqlite URI `file:...?mode=ro`
   безопасны параллельно с WAL.
7. Плагин не активен после установки: проверить `memory.provider: ob2h` в config.yaml и
   `ob2h plugin status`; провал prefetch НЕ роняет агент — падение в тихий Mode 0.

## Проверка работоспособности (read-only, без LLM)
`memory_search`, `memory_context`, `omnes_stats`, `graph_stats`, `dream_status`,
`workspace_read`, `ob2h sync status`. `omnes_backup` — безопасно, пишет в `data/backups/`.
Полный изолированный тест LLM-пайплайна: `references/testing.md`.

## Транскрипт живой сессии для knowledge_extract
`session_search` видит только завершённые сессии. Текущую брать из
`C:/Users/ipres/AppData/Local/hermes/state.db` (таблицы `sessions`, `messages`; роли
user/assistant; отфильтровать `[System:` и пустые; tool-выводы пропускать). Скрипт:
`references/testing.md`.

## Ссылки
- `references/testing.md` — транскрипт из state.db + JSON-RPC драйв сервера (изолированный OB2H_DATA_DIR)
- `references/autodream-and-history.md` — устройство дрима/истории
- План v0.8 (плагин+синк): `C:\Projects\omnesbot_for_hermes\docs\PLAN_v0.8.md`
