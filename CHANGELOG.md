# Changelog

Формат: Keep a Changelog (упрощённый). Версии — по мере появления пользовательского
контракта (MCP-инструментов).

## [0.1.0] — 2026-08-18

Первая рабочая версия: локальное MCP-хранилище знаний для Hermes (stdio).

### Added
- **Ядро хранения** (`config/db/vector/embedding`): SQLite WAL с версионными
  миграциями, FTS5-trigram (русский из коробки), вектора BLOB+numpy с косинусным
  поиском, провайдеры эмбеддингов fastembed (CPU, без torch) и OpenAI-совместимый API.
- **Память**: гибридный поиск FTS+вектор со слиянием RRF k=60, важность с затуханием
  и очисткой слабых, блок `<agent_memory>` для промпта (порт MemoryService).
- **Workspace**: MEMORY/SOUL/USER.md, history.jsonl с курсорами, git-история правок
  с восстановлением (порт MemoryStore + GitStore).
- **Консолидация сессий**: триггер по бюджету токенов, суммаризация LLM с raw-фолбэком,
  инструмент `session_log` (порт Consolidator).
- **Граф знаний KAG-lite**: чанкинг 3000/300 по предложениям, LLM-экстракция
  сущностей/отношений, инференс отношений по описаниям (~44 шаблона), фильтр мусора,
  дедуп-апсерт, гибридный поиск с 1-hop соседями, `graph_reason` с confidence
  (порт OneKE + KAGReasoningService, PG-путь).
- **Дриминг**: двухфазный Dream (анализ → агентный цикл точечных правок MD ≤10 итераций
  → git-коммит), AutoDreamWorker с гейтами 4ч/10 событий/lock(stale 1ч) и ретеншном
  daily-логов (порт Dream + AutoDreamWorker).
- **MCP-инструменты** (19): memory_save/search/update/forget/context,
  workspace_read/write, session_log, knowledge_extract, graph_search/reason/stats,
  dream_run/status/log/restore, omnes_stats/backup.
- **Служебное**: бэкапы VACUUM INTO + workspace с ротацией 14, CLI
  (`python -m omnes_memory.dream_cli run|status|backup`), логирование с ротацией.
- 102 теста (юнит + интеграционные через живой stdio MCP-клиент), ruff чисто.

## [0.0.1] — 2026-08-18

### Added
- Каркас проекта: план разработки, правила, docs, codegraph, pyproject.
