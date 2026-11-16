---
name: ob2h
description: Long-term memory, knowledge graph and dreaming backend of this Hermes agent. Use for any recall about the user, past conversations, projects, preferences, and for saving new important facts. Also use for developing/maintaining the ob2h project itself.
---

# OB2H — долговременная память этого агента (Rust, MCP + MemoryProvider)

## Поведение: память включена ВСЕГДА
- Каждый ход диалога попадает в ob2h автоматически: MemoryProvider-плагин
  (`sync_turn`/`session_ingest`), без плагина (Mode 0) — вызывай `session_log`
  после содержательных ответов сам, не дожидаясь просьбы.
- Вопросы о пользователе, его проектах, прошлых обсуждениях, предпочтениях → сначала
  recall (`memory_search` / блок `<agent_memory>`), потом ответ.
- Устные «запомни…» → `memory_save` с importance 0.8+; устойчивые факты — с `key`.
- Документы/длинные тексты → `knowledge_extract`; вопросы о связях «кто/что/чем связано» →
  `graph_reason`. Ночью/по гейтам авто-дрим обновляет MEMORY/SOUL/USER.md и граф.
- **Конец содержательной сессии (ВСЕГДА, не опция)** → `session_ingest` транскрипта
  (пары {role: user|assistant, content}, session_id для дедупа): в Mode 0 — обязательно,
  с плагином дубликат безвреден (дедуп по позиции). Дрим ест только history.jsonl
  (сводки консолидатора — порог ~84К символов на сервер-процесс) и с Ф78 — daily-логи
  напрямую: без записей в daily/history дрим голодает и память не консолидируется.
- В памяти встречаются факты с ДРУГОЙ машины пользователя (origin=pc/vps): пути и среды
  с чужого origin не существуют локально — используй их как контекст, а не как цели
  для файловых операций.

## Как вызывать память, когда прямых инструментов нет (MCP-коннектор)
- Часть сессий видит ob2h-инструменты как НЕ зарегистрированные: вызов `memory_save`
  /`memory_search`/`graph_search`/`omnes_stats` напрямую возвращает `Unknown tool`.
  Рабочий путь — коннектор: `mcp__ob2h__server_invoke_tool(tool_name=..., tool_input={...})`,
  схему конкретного инструмента брать через `mcp__ob2h__server_get_tool_schema`
  (она же перечисляет ВСЕ доступные имена инструментов сервера).
- Кириллица в `tool_input` через коннектор проходит нормально — отдельный JSON-RPC-драйв
  к `ob2h serve` нужен только если коннектор сам отвалился с ошибкой транспорта.
- Проверяй запись ЧТЕНИЕМ обратно (`memory_search` по ключу), а не по ответу `saved`.

## Полный текст записи и замена устаревших фактов
- `memory_search` отдаёт сниппеты ~200 символов — для правки существующей записи этого
  мало. Полный текст брать из БД read-only: `sqlite3.connect("file:<data>/ob2h.db?mode=ro", uri=True)`,
  таблица `memories` (колонки `key, content, category, importance, trust, origin, deleted_at`).
- Устаревший факт НЕ удалять: `memory_update(key=...)` с текстом «оригинал целиком +
  пометка [УСТАРЕЛО <дата>: заменено — см. <новый ключ>]». История сохраняется, а recall
  больше не тянет мёртвый адрес без оговорки.
- Один факт = одна запись с осмысленным `key` (напр. `hmem-<тема>-<дата>`); зеркальные
  копии `m_*` (cat=general) создаёт сам плагин из MEMORY.md — их не править руками.

## Режимы подключения (не путать при диагностике)
| Режим | Как выглядит | Что важно |
|---|---|---|
| **A: плагин (целевой)** | `memory.provider: ob2h` в config.yaml, плагин в `{{HERMES_HOME}}/plugins/ob2h/` | Один процесс ob2h serve (спавнит плагин); захват/recall происходят САМИ; индикатор 🧠 = prefetch сработал; `mcp_servers.ob2h` следует удалить |
| **B: переходный** | плагин И `mcp_servers.ob2h` одновременно | Работает (WAL+busy_timeout+dream-lock), но два процесса — лучше уйти в A |
| **0: MCP-only (фолбэк)** | только `mcp_servers.ob2h` в config.yaml | Захват только через явные вызовы инструментов |

## Что это
- Один Rust-бинарник: MCP-сервер stdio + SQLite WAL + FTS5-trigram (русский)
  + локальные Candle-эмбеддинги `paraphrase-multilingual-MiniLM-L12-v2` (384d,
  кэш `~/.cache/huggingface/hub/`); LLM для экстракции/дрима — DeepSeek.
- Пути этой машины: проект `{{PROJECT_DIR}}`, бинарник `{{BINARY}}`,
  данные/БД `{{DATA_DIR}}` (ob2h.db, workspace/: SOUL.md/USER.md/memory/MEMORY.md +
  daily/*.jsonl — файлы создаются лениво, `workspace_read` отсутствующего = `""`),
  Hermes home `{{HERMES_HOME}}` (config.yaml, plugins/ob2h, ob2h.json).
- Инструменты (36 шт): memory_save/search/update/forget/context, memory_feedback (trust ±),
  memory_merge (слияние дублей), workspace_read/write, session_log, session_ingest,
  knowledge_extract, graph_search (mode=classic|ppr)/reason (scope=docs|memory|all)/stats,
  dream_run/status/log/restore, omnes_stats/backup, project_init/scan/scan_status/
  context/graph_search/report/impact, project_call_path (callers/callees/call-path),
  ralph_start/iteration/verdict/context/report, ast_diff, ast_history.
- Детерминированный AST-граф кода: `project_scan` извлекает классы, функции, трейты и связи
  со 100% точностью (Graphify-подход) без вызовов LLM; скан идёт в фоне и не упирается
  в таймаут MCP-клиента — по умолчанию инструмент ждёт до 45 с, долгий скан отвечает
  «still running», результат досматривается `project_scan_status` (там же сводка из БД).
  CLI для этого агенту не нужен. `project_report` и `project_context`
  выделяют центральные хабы архитектуры (God Nodes).
  CLI для этого агенту не нужен. `project_report` и `project_context`
  выделяют центральные хабы архитектуры (God Nodes).
- Регрессионный bench кодового графа (v1.7.1): `ob2h bench --mode code --project <id>`
  против `data/bench/golden_code.jsonl` — recall/MRR PPR-поиска; используется для
  обоснования изменений PPR-весов (вердикт 2026-10-03: дефолт не менять — выдача
  не зависит от весов кодовых рёбер, цели находятся на этапе сидов).
- Форматы сканера (v1.7): 31 расширение кода (rs/py/ts/tsx/js/jsx/go/sql/c/cpp/cc/cxx/
  h/hpp/hxx/php/dart/java/cs/kt/kts/swift/rb/vue/svelte/scala/lua/ex/exs/sh/bash) +
  веб-разметка и стили (html/htm, css/scss, blade), контракты (proto, graphql/gql,
  openapi.json|yaml), IaC (Dockerfile*, .tf/.hcl, docker-compose|compose.yml|yaml,
  k8s|deploy|helm|chart*/*.yaml|yml), манифесты (Cargo.toml, package.json,
  pyproject.toml, requirements.txt, go.mod, pubspec.yaml, composer.json; lock-файлы
  исключены), спец-языки (.ipynb без outputs, .sol, .zig, .s/.S/.asm — ассемблер
  GAS/NASM/MASM, v2.0.1). Импорты внешних пакетов
  сливаются с манифестами в узлы `pkg:<eco>:<name>` (версия в description).
- Полезные запросы v1.7: `project_graph_search("pkg:rust:serde_json")` — пакет и его
  потребители; `project_graph_search("cssclass:<имя>")` — где определён CSS-класс;
  `project_impact("openapi.yaml")` / `project_impact("Dockerfile")` / `project_impact("Cargo.toml")`
  — что заденет правка контракта/образа/зависимости (v1.9.11: аргумент `min_confidence`,
  дефолт 0.5 — включает эвристики виртуальных вызовов и co-change `CO_CHANGES_WITH`;
  0.7 — только точные рёбра; в ответе блок «🧪 Затронутые тесты (TESTED_BY)» — что
  прогонять после правки); `project_call_path` проходит
  фронт → `route:` → хендлер (INFERRED-мосты SERVES_ENDPOINT/PROVIDES_RPC).
- AutoDreamWorker: гейты ≥4ч и ≥10 событий, lock, git-история правок, бэкапы
  (full/quick, verify, раздельная ротация), ночной bench-гейт (откат дрима при
  деградации recall/MRR). CLI: `ob2h stats | doctor [--fix] | dream run/status/log/restore |
  bench [--mode history] | backup [--scope quick] [verify <path>] |
  db quantize-embeddings | memory dedup [--dry-run] | install/uninstall |
  plugin install/uninstall/status | skill-install | agent install/status |
  update check/apply/status (v1.8.0, ADR-15) |
  project init/scan/list/report/dead-code/repo-map |
  ralph findings-to-memory | sync status/verify/export/import/apply-inbox/push/pull`.

## Синхронизация двух машин (PC ↔ VPS)
- Обмен — gzip-дельта-бандлы v2 JSONL поверх SSH (`{{DATA_DIR}}/sync/peers.json`:
  origin=pc|vps, приоритеты, пути; инициатива на PC — push/pull, VPS — apply-inbox+export).
  Только изменённое (курсор по updated_at); в бандле память+trust+memory_links+удаления;
  конфликты — LWW по updated_at для content (не молчаливый: журнал `sync/conflicts.jsonl`) + поле-уровневый merge метаданных;
  `ralph_*` вне бандлов (ADR-K6). Идемпотентно; сверка дрейфа — `ob2h sync verify`.
- Живую БД `{{DATA_DIR}}/ob2h.db` НИКОГДА не синкать файловой синхронизацией (WAL = порча),
  только папку бандлов. Диагностика обмена: `ob2h sync status`.

## Критичные питфоллы
1. **OB2H_LLM_API_KEY = ИМЯ env-переменной** с ключом (напр. `DEEPSEEK_API_KEY`), не сам
   ключ (развязка в коде, коммит 82d7e3f; фолбэк на литерал и на `<data_dir>/.env`).
   Симптом обратного: 401 `Your api key: ****_KEY is invalid`.
2. **«chunks записались, entities=0»** в knowledge_extract — упавший LLM-вызов (см. п.1);
   теперь инструмент сам возвращает `[Error]/[Подсказка]` с числом упавших чанков.
3. **Снабжение LLM — три эшелона** (2026-11): явный `OB2H_LLM_*` → основная LLM
   harness-системы (автодетект по `clientInfo` при `initialize`: hermes/zcode/codex
   читают свой конфиг; форс — `OB2H_LLM_HARNESS`) → локальная мини-модель
   `Qwen/Qwen3-0.6B` (~1.4 GB, качается при первом вызове; `OB2H_LOCAL_LLM=0` — выкл).
4. **Логи**: `{{DATA_DIR}}/logs/ob2h.log` (cwd сервера от Hermes = домашняя папка — до фикса
   82d7e3f логи могут лежать в `~/logs`, искать оба места).
5. **Пересборка (Windows)**: exe блокируется, пока процесс жив (Hermes/плагин держит).
   `cargo build --release` падает `os error 5` → сначала `Get-Process ob2h | Stop-Process
   -Force` (убьёт MCP в текущей сессии Hermes — спросить пользователя).
   **Пересборка (Linux/VPS)**: `cd <репо> && git pull && cargo build --release &&
   install -m755 target/release/ob2h /usr/local/bin/ob2h` — замена живого бинарника
   безопасна, подхватится после рестарта гейтвея (`systemctl --user restart
   hermes-gateway`).
5. **ob2h ТОЛЬКО для этого Hermes**: не добавлять другим агентам, не оборачивать в
   mcp-compressor (пользователь запретил явно).
6. Живой сервер держит БД; read-only проверки через sqlite URI `file:...?mode=ro`
   безопасны параллельно с WAL.
7. Плагин не активен после установки: проверить `memory.provider: ob2h` в config.yaml и
   `ob2h plugin status`; провал prefetch НЕ роняет агент — падение в тихий Mode 0.
8. **project_* — всегда передавай `id` явно.** Без `id` сервер молча подставляет
   «активный проект» своей сессии (Zero-Config-привязка к воркспейсу IDE): скан/поиск
   уходят в чужой проект и возвращают 0 результатов. Симптом: в логе есть
   «Зарегистрирован проект …», но нет «Начало AST-сканирования проекта '<id>'»,
   в БД `projects.last_scanned_at=NULL`. Долгий скан через `project_scan` не падает
   по таймауту: ответ «still running» → полли `project_scan_status id=<id>`, пока
   не придёт «job: done» (или «job: failed» с причиной).
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

13. **Сравнение/слияние memory_links — только по ключам через JOIN**: таблица ссылается
   на rowid `memories.id`, который у сторон НЕ совпадает. Запрос на обеих сторонах:
   `SELECT m1.key, m2.key, l.kind, l.weight, l.created_at FROM memory_links l
   JOIN memories m1 ON m1.id=l.from_id JOIN memories m2 ON m2.id=l.to_id
   WHERE l.deleted_at IS NULL`; разницу переносить минимальным бандлом
   (`type: "mlink"`, поля `from_key`/`to_key`, origin отправителя) в обе стороны.
14. **После пересборки exe повторяй `plugin install` и `skill-install`** — плагин и скилл
   вшиты в бинарник (`include_str!`), файлы на диске обновляет только установка из НОВОГО
   exe. Точные команды (из корня репо, с явным env — иначе `plugin install` перепинит
   `ob2h.json` на `cwd/data`): `OB2H_DATA_DIR={{DATA_DIR}} "{{BINARY}}" plugin install` и
   `"{{BINARY}}" skill-install` — подкоманда именно **`skill-install`** (`ob2h skill` →
   `unrecognized subcommand 'skill'`; в AGENTS.md §5 опечатка «ob2h skill install»).
   Сверка файлов с репо — `diff --strip-trailing-cr` (в репо LF, `cmp` даёт ложный DIFF).
   **`skill-install` ПЕРЕЗАПИСЫВАЕТ установленный SKILL.md шаблоном, вшитым в бинарь** —
   правки, сделанные руками прямо в `~/.hermes/skills/devops/ob2h/SKILL.md`, молча
   теряются: всё ценное вносить в `skills/ob2h/SKILL.md` репо и коммитить.
   **Карта локальной установки:** Hermes-плагин и MCP-сервер работают из `{{BINARY}}`
   (эталон, закреплён в `ob2h.json`); копия в PATH (`~/.cargo/bin/ob2h.exe` — её берут
   `ob2h` в шелле и MCP-конфиг Antigravity) обновляется ВРУЧНУЮ копированием exe.
   Релизный zip содержит ТОТ ЖЕ exe, что собран в `target/release/` (сверять sha256),
   поэтому «обновить локально до версии X» = скопировать exe из `target/artifacts/*.zip`
   или `target/release/` + прогнать установщики + `ob2h --version`.
15. **Варианты `skills/<агент>/SKILL.md` не темплейтятся** (только корневой ob2h проходит
   skill-install): при переезде репо/store правь пути exe и env `OB2H_DATA_DIR` в каждом
   варианте И в живых MCP-конфигах агентов, учитывая оба написания: `X:\...\target\release\ob2h.exe`
   и `X:/.../data` (прямой слеш в env-примерах).
16. **lesson_save vs memory_save (v1.9.9):** урок — процедурный опыт («что делать / чего
   избегать / когда применимо», confidence, дедуп по fingerprint — повтор укрепляет, а не
   плодит дубли); факт о пользователе/проекте — по-прежнему `memory_save`. Не путать:
   «всегда запускай тесты перед коммитом» → урок; «проект X на Rust» → память.
   Слоты (`slot_*`) — редактируемый рабочий контекст агента, не факты: лимит размера
   1..20000 символов, append при переполнении — ошибка 413 с подсказкой (заменить через
   `slot_replace`); в контекст.injectятся только pinned+непустые в бюджете
   `OB2H_SLOTS_MAX_CHARS` (дефолт 1500).
17. **Показать память владельцу → `viewer_open` (v1.9.9):** локальное окно просмотра
   (память/граф/сессии/уроки/слоты/статистика/секреты), read-only, отдельный detached-процесс.
   Headless/SSH/сборка без gui — окно недоступно: сообщить честно (`[Error] … [Подсказка]`),
   фолбэк — CLI (`ob2h stats`, `ob2h audit`, `ob2h secrets status`). Если harness убивает
   job-дерево и окно не выживает — владелец запускает вручную: `ob2h viewer`.
18. **Секреты во viewer (v1.9.9):** вкладка «Секреты» в окне просмотра — init («Начать
   сохранять», PIN дважды), unlock (4 цифры, Enter), список/добавление/reveal (авто-скрытие
   15 с)/копирование/удаление. PIN вводится ТОЛЬКО владельцем в окне — правило 1 ниже
   действует и здесь; агент не запрашивает PIN в чате никогда.

## Проверка работоспособности (read-only, без LLM)
`memory_search`, `memory_context`, `omnes_stats`, `graph_stats`, `dream_status`,
`workspace_read`, `ob2h sync status`. `omnes_backup` — безопасно, пишет в
`{{DATA_DIR}}/backups/`. Полный изолированный тест LLM-пайплайна: `references/testing.md`.

## Транскрипт живой сессии для knowledge_extract
`session_search` видит только завершённые сессии. Текущую брать из
`{{STATE_DB}}` (таблицы `sessions`, `messages`; роли user/assistant; отфильтровать
`[System:` и пустые; tool-выводы пропускать). Скрипт: `references/testing.md`.

## Ссылки
- `references/testing.md` — транскрипт из state.db + JSON-RPC драйв сервера (изолированный OB2H_DATA_DIR)
- `references/autodream-and-history.md` — устройство дрима/истории
- `references/coding-graph.md` — кодовый граф (Ф36–Ф40): точки read-path, правила расширения
  сканера новым языком/форматом, деплой бинарника на живую БД
- Планы: `{{PROJECT_DIR}}/PLAN_v1.3.md`, `{{PROJECT_DIR}}/PLAN_v1.4.md`, `{{PROJECT_DIR}}/PLAN_v1.6.md`
  (System One слой: Laya за флагами OB2H_S1_*, дефолт off — реранк memory_search, дрим-гейты,
  инструмент s1; черновик rev.1, ждёт ревью владельца); скилл Ralph-циклов — `ralph-loop`

## Автообновление (v1.8.0, ADR-15) — известное ограничение
- `ob2h update check|apply|status` ходит ТОЛЬКО в GitHub API (`api.github.com`,
  Accept `application/vnd.github+json`), но дефолтный `OB2H_UPDATE_REPO=Pofium/ob2h`
  на GitHub не существует (там репозиторий `Pofium/ob2h`) → `update check` из коробки
  падает `404 Not Found`. Обход без правок кода: `OB2H_UPDATE_REPO=Pofium/ob2h ob2h update check`
  (проверено 10.10.2026 — видит v1.8.0). Релизы на GitVerse апдейтер НЕ видит: у GitVerse
  свой хост `api.gitverse.ru` и свой Accept-хедер — поддержки нет.

## Хранилище секретов (Secrets Vault, v1.9)
- Зашифрованный сейф для ключей, токенов и паролей на базе Argon2id + XChaCha20-Poly1305 (схема M9).
- **Правило 1 (Ввод PIN):** Пин (ровно 4 цифры) вводится ТОЛЬКО пользователем локально — в графическом окне PIN (`egui`, ADR-23) или в терминале (`ob2h secrets unlock`/`init`/`rekey`) без эха. Агенту ЗАПРЕЩЕНО запрашивать PIN в чате или пытаться передавать его в аргументы инструментов.
- **Правило 2 (Явный запрос):** Доступ к секрету (`secrets_get(name)`) вызывается только при явной команде пользователя («используй токен X для вызова API»).
- **Правило 3 (Изоляция):** Модели строго ЗАПРЕЩЕНО сохранять расшифрованные секреты в память (`memory_save`), заметки (`workspace_write`) или выводить их в открытый чат без необходимости.
- Инструменты MCP: `secrets_status`, `secrets_set`, `secrets_get`, `secrets_delete`, `secrets_lock`.

## Жизненный цикл памяти и окно просмотра (v1.9.9)
- **Уроки (lessons):** `lesson_save` (content, context?, confidence?, project?, tags?, source?)
  → created|strengthened; `lesson_recall` (FTS × confidence × recency), `lesson_list`,
  `lesson_delete` (tombstone). Дрим делает недельный decay-sweep БЕЗ автоудалений.
- **Слоты (slots):** `slot_list/get/create/append/replace/delete`; project-слот затеняет
  global; `memory_context` и ресурс `memory://context` добавляют блок `<agent_slots>`
  (pinned+непустые, алфавит, бюджет `OB2H_SLOTS_MAX_CHARS=1500` по границе целого слота).
- **Хронология/рекап/провенанс/аудит:** `memory_timeline` (окно daily-логов вокруг якоря),
  `session_recap` (инкрементальный дайджест; save=true → память с meta.source_ids),
  `memory_verify` (трассировка факта к цитатам daily-логов), `memory_audit` (леджер операций;
  значения секретов в леджере не хранятся — только имена). CLI: `ob2h audit`, `ob2h lessons`.
- **Скрабинг:** `session_log`/`session_ingest` маскируют токены/ключи и имена записей сейфа
  перед daily-логом (`OB2H_REDACT=0` — выкл).
- **Окно просмотра (viewer):** MCP `viewer_open` (56-й инструмент; статусы opened /
  already_running / gui_unavailable) либо CLI `ob2h viewer [--data-dir …]`. Окно read-only
  («ни одной записи»; единственное исключение — вкладка «Секреты» по локальному PIN),
  single-instance (lockfile `data/viewer.lock`), авто-обновление 2 с + F5, RU/EN
  (`OB2H_UI_LANG`), тёмная/светлая тема. Сборка: `cargo build --release --features gui`;
  headless-сборка отвечает честным `[Error] … [Подсказка]`. `ob2h doctor` показывает
  «Viewer: доступен / сборка без gui».

## Новые возможности v2.0.0
- **Сверка конфликтов знаний:** инструмент `knowledge_reconcile` (dry-run предпросмотр или apply для устранения противоречий с рёбрами `SUPERSEDES`/`CONTRADICTS`).
- **Токен-бюджетный AST-скелет:** параметр `max_tokens` в `graph_reason` и `memory_context` для компактной выдачи «файл → сигнатура → зависимости» с Elbow cutoff.
- **Обслуживание CLI:** `ob2h migrate --dry-run` (предпросмотр миграций с авто-бэкапом), `ob2h doctor --fix` (preflight + очистка lock/битых конфигов), `ob2h self uninstall [--keep-data | --purge-data]`.
