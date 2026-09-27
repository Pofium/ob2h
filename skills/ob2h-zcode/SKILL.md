---
name: ob2h
description: "Долговременная память, граф знаний, AST-граф кода и дриминг (OB2H). Используй для recall о пользователе, проектах и прошлых решениях перед ответом; для сохранения важных фактов; для анализа архитектуры проектов (project_*), поиска по кодовому графу, blast radius и workspace-файлов агента (MEMORY/SOUL/USER)."
---

# OB2H — долговременная память и граф кода этого агента (Rust, MCP)

> ZCode-вариант скилла. Исходники: `skills/ob2h-zcode/SKILL.md` (этот файл, пути
> этой машины) и `skills/ob2h/SKILL.md` (Hermes, с `{{шаблонами}}`) — общий
> контент править синхронно в обоих. Установка в ZCode — вручную:
> `cp skills/ob2h-zcode/SKILL.md ~/.zcode/skills/ob2h/SKILL.md`
> (`ob2h agent install --agent zcode` регистрирует только MCP, скилл не деплоит).

## Пути этой машины
- Бинарник: `C:\Projects\ob2h\target\release\ob2h.exe` (репозиторий переехал из
  `C:\Projects\omnesbot_for_hermes`; рабочая копия `~/.cargo/bin/ob2h.exe` — НЕ хардлинк,
  обновлять вручную после пересборки: `cp target/release/ob2h.exe ~/.cargo/bin/`)
- MCP-регистрация (ZCode): `~/.zcode/cli/config.json` → `mcp.servers.ob2h` — stdio, args `["serve"]`,
  `cwd=C:\Projects\Omnes-agent`, `env.OB2H_DATA_DIR=C:\Projects\ob2h\data`
  (18.09.2026 command исправлен на `C:\Projects\ob2h\...` — старый путь не существовал,
  из-за этого MCP не поднимался и всё шло через CLI). Применяется после рестарта сессии.
  Также `~/.zcode/mcp.json` (глобальный) указывает на ту же БД.
  **Не оборачивать в mcp-compressor** (пользователь запретил явно).
- Данные/БД: `C:\Projects\ob2h\data` (единая память всех агентов: `ob2h.db`,
  `workspace/` — SOUL.md/USER.md/memory/MEMORY.md + daily/*.jsonl — файлы создаются лениво,
  `workspace_read` отсутствующего = `""`; `backups/`, `logs/`). С 05.09.2026 это ОБЩАЯ память —
  инстанс `C:\Projects\Omnes-agent\data` слит сюда и удалён (origin=omnes в БД).
- Исходники: `C:\Projects\ob2h` (доки: `docs/ARCHITECTURE.md`, `docs/SYNC.md`,
  `docs/REFERENCE_omnesbot.md`, `PLAN_v1.6.md` в корне)
- Изменение конфига MCP или скилла → нужен рестарт сессии ZCode (подключение — на старте сессии)

## Поведение: память включена ВСЕГДА
- В ZCode плагина MemoryProvider нет (всегда «Mode 0»: MCP-only) — захват происходят ТОЛЬКО
  через явные вызовы: после содержательных ответов вызывай `session_log` сам, не дожидаясь просьбы.
- Вопросы о пользователе, его проектах, прошлых обсуждениях, предпочтениях → сначала
  recall (`memory_search` / `memory_context`), потом ответ.
- Устные «запомни…» → `memory_save` с importance 0.8+; устойчивые факты — с `key`.
- Документы/длинные тексты → `knowledge_extract`; вопросы о связях «кто/что/чем связано» →
  `graph_reason`. Ночью/по гейтам авто-дрим обновляет MEMORY/SOUL/USER.md и граф.
- В памяти встречаются факты с ДРУГОЙ машины пользователя (origin=pc/vps): пути и среды
  с чужого origin не существуют локально — используй их как контекст, а не как цели
  для файловых операций.

## Инструменты (36 шт, префикс `mcp__ob2h__`)
- **Память**: `memory_save` (`content`, опц. `key`, `importance` 0..1, `project_id`, `tags`) /
  `memory_search` (`mode`: hybrid — дефолт FTS+вектор RRF | fts | vector |
  graph — PPR-расширение по memory_links; `project_id`) / `memory_update` / `memory_forget` /
  `memory_feedback` (верdict helpful/unhelpful/outdated — правит trust записи) /
  `memory_merge` (явное подтверждённое слияние почти-дублей; авто-слияние запрещено) /
  `memory_context` (блок `<agent_memory>` для промпта)
- **Граф знаний**: `knowledge_extract` (из `text` или `file`; txt/md/pdf/docx) /
  `graph_search` (`mode`: classic — дефолт, 1-hop соседи | ppr — multi-hop Personalized
  PageRank; фильтры `project_id`, `provenance`) / `graph_reason` (ответ с уверенностью
  и цепочкой; `scope`: docs | memory — PPR по памяти | all) / `graph_stats`
- **Проекты (AST-граф кода)** — аргумент проекта называется **`id`** (не `project_id`)
  и во ВСЕ вызовы передаётся ЯВНО: без `id` сервер молча подставит «активный проект»
  сессии (Zero-Config по воркспейсу IDE) — скан/поиск уйдут в чужой проект (питфолл 8):
  `project_init` (`id`,`name`,`path`, опц. `description`,`tech_stack`) / `project_scan`
  (`incremental` по SHA256, дефолт true; скан идёт ФОНОМ и таймаута MCP не боится:
  по умолчанию ждёт до 45 с, долгий скан ответит «still running» — тогда полли
  `project_scan_status`) / `project_scan_status` (`id` — job-статус скана + сводка
  из БД: ast_nodes/god_nodes/pending_embedding/last_scanned; агенту НЕ нужно
  уходить в CLI/логи/БД) / `project_context` (`id`, опц. `query` под задачу;
  `mode=repo_map` — карта «файл → символы» под token-бюджет) /
  `project_graph_search` (`id`,`query`; `mode` hybrid|text|vector; `provenance` ast|llm) /
  `project_report` / `project_impact` (`symbol_or_path`, `depth` 1..10 — blast radius) /
  `project_call_path` (явная цепочка вызовов/зависимостей from→to; без `to_symbol` —
  `mode` callers|callees)
- **Ralph-циклы разработки** (цикл по спеке из openspec/changes): `ralph_start` /
  `ralph_iteration` (гипотеза/план/результат → авто-вердикт ТОЛЬКО по объективным
  тестовым сигналам) / `ralph_verdict` (смена вердикта человеком/дримом) /
  `ralph_context` (контекст-пакет на задачу: спека/негатив/reuse/инварианты) /
  `ralph_report` (прогресс, debt-леджер, gain-метрики)
- **AST-дельты**: `ast_diff` (символьный дифф между итерациями/коммитами) /
  `ast_history` (хронология изменений символа)
- **Workspace и дриминг**: `workspace_read` / `workspace_write` (memory|soul|user|history; запись = git-коммит) /
  `session_log` / `session_ingest` (bulk-транскрипт) / `dream_run` / `dream_status` / `dream_log` / `dream_restore`
- **Служебные**: `omnes_stats`, `omnes_backup` (VACUUM INTO + workspace, ротация 14)

Детерминированный AST-граф: `project_scan` извлекает классы, функции, трейты и связи со 100%
точностью (Graphify-подход) без LLM; `project_report`/`project_context` выделяют хабы (God Nodes).
AutoDreamWorker: гейты ≥4ч и ≥10 событий, lock, git-история правок, ночной bench-гейт
(откат дрима при деградации recall/MRR).

## Рабочий процесс в ZCode
1. **Начало задачи** → `project_context` (id + описание задачи) и `memory_context` — до чтения файлов.
2. **Перед правкой** → `project_impact` по символу/файлу; после крупных рефакторингов → `project_scan` заново.
3. **«Как это устроено»** → `project_graph_search` / `project_call_path`; факты вне кода → `memory_search`/`graph_search`.
4. **Значимое решение** → `memory_save` (с `project_id`); перед этим `memory_search` — не дублировать.
5. **Долгая сессия завершена** → `session_ingest` транскрипта; по гейтам дрим консолидирует сам.

## Синхронизация двух машин (PC ↔ VPS)
- Обмен — gzip-дельта-бандлы v2 JSONL поверх SSH (`C:\Projects\ob2h\data\sync\peers.json`:
  origin=pc|vps, приоритеты, пути; инициатива на PC — push/pull, VPS — apply-inbox+export
  по таймеру). Только изменённое (курсор по updated_at); в бандле память+trust+memory_links+
  удаления; поле-уровневый merge с журналом `sync/conflicts.jsonl` (не молчаливый LWW);
  `ralph_*` вне бандлов (ADR-K6). Идемпотентно; сверка дрейфа — `ob2h sync verify`.
- Живую БД `ob2h.db` НИКОГДА не синкать файловой синхронизацией (WAL = порча),
  только папку бандлов. Диагностика обмена: `ob2h sync status`.

## Критичные питфоллы
1. **OB2H_LLM_API_KEY = ИМЯ env-переменной** с ключом (напр. `DEEPSEEK_API_KEY`), не сам ключ.
   Симптом обратного: 401 `Your api key: ****_KEY is invalid`.
2. **«chunks записались, entities=0»** в `knowledge_extract` — почти всегда упал LLM-вызов (см. п.1).
3. **Логи**: `C:\Projects\ob2h\data\logs\ob2h.log` (cwd сервера от ZCode — `C:\Projects\Omnes-agent`;
   до фикса 82d7e3f логи могли лечь в `~/logs` — искать оба места).
4. **Пересборка (Windows)**: exe блокируется, пока процесс жив. `cargo build --release` падает
   `os error 5` → сначала `Get-Process ob2h | Stop-Process -Force` (убьёт MCP в текущей сессии —
   спросить пользователя). **(Linux/VPS)**: замена живого бинарника безопасна, подхватится после рестарта.
5. **Не добавлять ob2h другим агентам без ведома пользователя, не оборачивать в mcp-compressor.**
6. Живой сервер держит БД; read-only проверки через sqlite URI `file:...?mode=ro` безопасны параллельно с WAL.
7. ZCode-специфика: смена `mcp.servers.ob2h` в `~/.zcode/cli/config.json` применяется только после
   рестарта сессии; статус — Settings → MCP. Хранилище закреплено через `OB2H_DATA_DIR` —
   CLI-команды ob2h запускать с тем же `OB2H_DATA_DIR=C:\Projects\ob2h\data`,
   иначе данные уйдут в чужую БД. Отдельного инстанса Omnes-agent больше нет.
8. **project_* — всегда передавай `id` явно.** Без `id` сервер молча подставляет
   «активный проект» своей сессии (Zero-Config-привязка к воркспейсу IDE): скан/поиск
   уходят в чужой проект и возвращают 0 результатов. Симптом: в логе есть
   «Зарегистрирован проект …», но нет «Начало AST-сканирования проекта '<id>'»,
   в БД `projects.last_scanned_at=NULL`. Долгий скан через `project_scan` не падает
   по таймауту MCP-клиента (фоновый job): ответ «still running» → полли
   `project_scan_status id=<id>` до «job: done» / «job: failed». Статус job'а живёт
   в процессе сервера: после его рестарта «job: none in this server session» —
   тогда смотреть на db-часть ответа (last_scanned/ast_nodes).
9. **Синк после перерыва**: watermark (sync_state.last_export_at) может «перепрыгнуть»
   записи, вставленные ПОСЛЕ последнего экспорта, но ДО его завершения — они выпадают
   из дельт навсегда. Диагностика: сверка ключей памяти обеих сторон read-only sqlite
   (`file:...?mode=ro`), лечение: откат курсора в sync_state на дату пропуска + обычный
   (НЕ --full) экспорт.
10. **`sync export --full` на VPS с RAM < 8 ГБ = OOM-kill (exit 137)**: полный бандл
    несёт весь граф (600k+ узлов). Карантин: `data/sync/oversize/` вне inbox, чтобы
    ночной apply-inbox не падал. Дельту за месяц тоже может раздуть граф — если нужен
    только перенос памяти, собрать минимальный бандл вручную (header + строки mem/mlink),
    **уникальный bundle_id обязателен** — скопированный из эталона даст no-op «уже применён».
11. **`sync verify` требует в peers.json пира `data_dir` и `bin`** (удалённая сторона
    выполняет `OB2H_DATA_DIR=<data_dir> <bin> sync local-stats`); без них — ошибка.
12. **При импорте на другой стороне через scp** — файл кладётся в inbox пира,
    применяется его `sync apply-inbox`; прямой `sync import <файл>` на удалённой машине
    тоже работает.

## Проверка работоспособности (read-only, без LLM)
`memory_search`, `memory_context`, `omnes_stats`, `graph_stats`, `dream_status`, `workspace_read`,
`ob2h sync status`. `omnes_backup` — безопасно (пишет в `data/backups/`; `--scope quick`,
`verify <path>`). Диагностика окружения: `ob2h doctor`.

## CLI (диагностика; всегда с `OB2H_DATA_DIR=C:\Projects\ob2h\data`)
`ob2h stats | doctor [--fix] | dream run/status/log/restore | bench [--mode history] |
backup [--scope quick] [verify <path>] | db quantize-embeddings | memory dedup [--dry-run] |
project init/scan/list/report/dead-code/repo-map | ralph findings-to-memory [--dry-run] |
sync status/verify/export/import/apply-inbox/push/pull`. Агенту почти всё это доступно
через MCP — CLI нужен для скриптов и диагностики вне сессии.
