# Changelog

Формат: Keep a Changelog (упрощённый). Версии — по мере появления пользовательского
контракта (MCP-инструментов).

## [1.0.0] — 2026-08-30

### Added
- **Проектная память и детерминированный AST-граф кода** (Фазы 10-13, вдохновлено Graphify):
  - Таблица `projects` в SQLite (миграция M3) с поддержкой изоляции памяти и сущностей по `project_id`.
  - Высокопроизводительный статический AST-парсер (`AstCodeExtractor`) для Rust, Python, TypeScript/JavaScript, Go, SQL. Извлекает классы, структуры, интерфейсы, трейты, функции, таблицы и связи (`DEFINES`, `IMPORTS`, `INHERITS`, `FOREIGN_KEY_TO`) со 100% точностью (`provenance: 'ast'`, `confidence: 1.0`) без расхода токенов LLM.
  - Графовая аналитика и выявление центральных узлов («God Nodes») по Degree & In-degree Centrality.
  - Сжатые архитектурные дайджесты (`project_report`) и блок `<project_context>` для системного промпта агента.
- **5 новых MCP-инструментов** (всего 24 инструмента):
  - `project_init` — регистрация и настройка проекта.
  - `project_scan` — детерминированное AST-сканирование кодовой базы и индексация в граф.
  - `project_context` — генерация архитектурного контекста с God Nodes для промпта агента.
  - `project_graph_search` — фильтрованный поиск по графу кода с указанием файлов и строк.
  - `project_report` — подробный Markdown-дайджест кодовой базы.
- **Мультиагентный установщик и CLI-менеджер (`ob2h agent`)**:
  - Поддержка подключения в 1 клик для всех популярных AI-ассистентов: Claude Code, Cursor, Windsurf, ZCode, Gemini CLI / Antigravity, Qwen Code, OpenCode, Hermes.
  - Команды `ob2h agent install [--agent <name>|--all] [--path <project_dir>]` и `ob2h agent status`.
  - Команды `ob2h project init/scan/list/report`.

## [0.9.0] — 2026-08-23

### Added
- **Синхронизация двух инстансов PC ↔ VPS** (ADR-9): инкрементальные gzip-бандлы JSONL
  поверх SSH/manual, `ob2h sync status/export/import/apply-inbox/push/pull`.
  LWW по `updated_at` + tie-break по приоритету `origin`, tombstones, идемпотентность
  по bundle_id, авто-бэкап перед каждым новым бандлом, эмбеддинги в бандле
  (или re-embed локальной моделью). Фаза автодрима `after_dream` (best-effort).
- **Миграция M2** (аддитивная): `origin`/`deleted_at`/`updated_at` +
  `sync_state`; авто-бэкап `pre-v08-*.db` перед миграцией живой БД; даунгрейт-безопасно.
- **Tombstones**: `memory_forget`/`purge_weak` реплицируют удаление, поиск и контекст
  фильтруют удалённое, физическая чистка — в maintenance автодрима (retention×2).
- **CLI `ob2h skill install`**: скилл из репо (`skills/ob2h/`) деплоится в Hermes
  с подстановкой путей машины (единый исходник Windows/Linux).
- Обвязка синка: systemd-таймер (`scripts/vps/`), Task Scheduler-скрипт
  (`scripts/pc/`), образец `peers.json`.
- SQLite `busy_timeout`, `origin=''`-семантика («строка этого узла»).

### Changed
- AGENTS.md/docs приведены к Rust-реальности; HERMES_INTEGRATION.md переписан
  (режимы 0/A/B, синк, обновление); ARCHITECTURE.md актуализирован (плагин, sync,
  M2-схема, 19 инструментов); новый гайд `docs/SYNC.md` (кому нужно, настройка,
  конфликты, безопасность); README — раздел синхронизации и сценарии.

## [0.8.0] — 2026-08-23

### Added
- **MCP-инструмент `session_ingest`** (19-й, в конец списка): массовая запись транскрипты
  сессии парами user/assistant в daily-лог с дедупом по `(session_id, позиция сообщения)`
  (kv-счётчик) — повторный вызов с полной транскриптой добавляет только хвост. Роли кроме
  user/assistant пропускаются. Контракт старых 18 инструментов не изменился (снапшот-тест
  `tools/list`).
- **MemoryProvider-плагин для Hermes** (`plugin/ob2h/`, Python stdlib-only): долгоживущий
  subprocess `ob2h serve` + JSON-RPC/stdio. Автоматически, без инициативы модели:
  `sync_turn` → `session_ingest` каждый ход; `on_session_end`/`on_pre_compress` → полная
  транскрипта; `queue_prefetch`/`prefetch` → инъекция `<agent_memory>` перед ходом
  (+ `recall_status` 🧠); `get_tool_schemas` → инструменты ob2h (кроме автоматических
  session_log/session_ingest/memory_search/memory_context); `on_memory_write` — зеркало
  builtin-памяти. Non-primary контексты (cron/subagent) не пишут. Рестарт subprocess
  с backoff, health-ping 60с, деградация без падения агента.
- **CLI `ob2h plugin install|uninstall|status`**: деплой плагина в `$HERMES_HOME/plugins/ob2h/`,
  `ob2h.json` пинит binary/data_dir; конфиг Hermes не правится — печатает сниппет
  `memory.provider: ob2h` для ручной вставки.
- SQLite `busy_timeout=5000` (Mode B: плагин + mcp_servers одновременно).

### Tests
- Rust: session_ingest (пары/дедуп/хвост/ошибки контракта), снапшот tools/list.
- Python: 16 тестов — RPC-клиент против фейк-сервера (handshake/таймаут/рестарт),
  провайдер (аккумулятор, gating, схемы, prefetch), интеграция с реальным `ob2h serve`.

## [0.1.1] — 2026-08-18

### Added
- **dream-extract**: во время дрима сущности и отношения извлекаются из новых
  записей сессий в общий граф (`OB2H_DREAM_EXTRACT_ENABLED`, по умолчанию вкл).
  Сессии и документы теперь populate один граф с дедупом по label|type.
- **Локальные эмбеддинги**: fastembed 0.8.0 установлен и проверен на Python 3.14;
  модель `paraphrase-multilingual-MiniLM-L12-v2` (0.22 ГБ, 384d) скачана и
  протестирована на русском (semantic-тест в `tests/test_embedding_local.py`,
  маркер `embeds`). Дефолт конфига исправлен с недоступного multilingual-e5-small.
- Документированы альтернативы эмбеддингов: LM Studio embeddinggemma-300m-qat
  (уже скачана у владельца) и Ollama mxbai-embed-large через `OB2H_EMBED_PROVIDER=api`.

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
  (`python -m ob2h.dream_cli run|status|backup`), логирование с ротацией.
- 102 теста (юнит + интеграционные через живой stdio MCP-клиент), ruff чисто.

## [0.0.1] — 2026-08-18

### Added
- Каркас проекта: план разработки, правила, docs, codegraph, pyproject.
