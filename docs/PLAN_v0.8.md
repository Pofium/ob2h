# План v0.8: постоянная интеграция с Hermes (MemoryProvider-плагин) и синхронизация двух инстансов

> Детальный план релиза **0.8.0**. Создан по заказу владельца 2026-08-23.
> Краткая выжимка фаз продублирована в `PLAN.md` §2 (фазы 7–9); этот документ — источник деталей.
> Главные требования: **(1)** захват диалогов и вспоминание работают постоянно, без инициативы
> модели; **(2)** обмен данными между двумя инстансами ob2h (PC ↔ VPS); **(3)** обновление
> с v0.7.1 не ломает существующую установку, данные и контракт MCP.

---

## 0. Диагноз v0.7.1 (что чиним)

| Проблема | Причина |
|---|---|
| Диалоги попадают в ob2h только когда Hermes сам вызовет `session_log` | Единственная точка входа диалогов — MCP-инструмент, вызов зависит от решения модели |
| Recall не происходит «сам» | `memory_context`/`memory_search` — тоже инструменты по инициативе модели; скилл `devops/ob2h` написан как скил разработки, не поведения |
| Автодрим простаивает (гейт ≥10 событий не набирается) | События daily-лога создаёт только `session_log` |
| Скилл и HERMES_INTEGRATION.md частично устарели | Переезд на Rust (v0.7.1) не отразился в docs/скилле полностью |

Ключевая находка (из исходников Hermes, `hermes-agent/`): у Hermes есть plugin-интерфейс
**MemoryProvider** — детерминированные хуки на каждый ход. Это устраняет зависимость
от «доброй воли» модели.

Ссылки по Hermes (актуально для установленной версии, проверено 2026-08-23):

- `agent/memory_provider.py` — ABC `MemoryProvider`: `initialize`, `system_prompt_block`,
  `prefetch`/`queue_prefetch`, `sync_turn`, `on_session_end`, `on_pre_compress`,
  `on_memory_write`, `get_tool_schemas`, `handle_tool_call`, `recall_status`, `shutdown`.
- `agent/memory_manager.py` — менеджер: инъекция `system_prompt_block` в системный промпт,
  `prefetch_all` → блок контекста перед ходом, `sync_turn` после хода, лимит «один внешний
  провайдер», скраббинг блоков из стрима.
- `run_agent.py:4465` — `queue_prefetch_all` на каждый user-промпт; `run_agent.py:4364` —
  `on_session_end(messages)` при завершении сессии. Тривиальные промпты («ок», «спасибо»)
  отсеиваются встроенным `TRIVIAL_PROMPT_RE` — шум не дергает recall.
- `plugins/memory/__init__.py` — discovery: бандл → **user** `$HERMES_HOME/plugins/<name>/` →
  project → pip entry points. Каталог провайдера = `__init__.py` с классом MemoryProvider.
  Активация — ключ `memory.provider: <name>` в `config.yaml`.
- Пример-референс: `plugins/memory/mem0/` (`plugin.yaml` + `__init__.py` + `_backend.py`).

---

## 1. Инварианты совместимости (сквозные, проверяются на каждой задаче)

Обновление v0.7.1 → v0.8.0 **не ломает**:

1. **Контракт MCP**: все 18 инструментов v0.7.1 сохраняют имена/аргументы/семантику.
   Новые инструменты только добавляются (в конец списка). Тест-снапшот `tools/list`
   на старые 18 обязателен (см. §7.5).
2. **Конфиг Hermes**: блок `mcp_servers.ob2h` продолжает работать без правок.
   Плагин — опция, включаемая вручную владельцем (`memory.provider: ob2h`).
3. **Данные**: `OB2H_DATA_DIR` не переезжает; миграция схемы только аддитивная
   (новые колонки/таблицы), перед миграцией — автоматическая копия БД
   в `data/backups/pre-v08-<date>.db`.
4. **Даунгрейт-безопасность**: бинарник v0.7.1 на мигрированной БД продолжает читать/писать
   (новые колонки имеют DEFAULT, старый код именует колонки явно). Покрывается тестом §8.5.
5. **CLI**: существующие подкоманды (`serve`, `dream …`, `backup`, `stats`, `install`,
   `uninstall`) не меняют поведение; новые — только добавляются.
6. **Скилл**: заменяется файл в `$HERMES_HOME/skills/devops/ob2h/` — это данные агента,
   не конфиг; замена разрешена владельцем (2026-08-23). Старые `references/*.md` остаются.

Версионирование: `0.8.0` (minor — всё аддитивно). `serverInfo.version` уже динамический.
CHANGELOG при релизе: добавить пропущенную запись 0.7.1 (Rust-переезд) + 0.8.0.

---

## 2. Общая архитектура после v0.8

```
Режим A (рекомендуемый, «plugin-only»):

Hermes (config.yaml)
  ├─ memory.provider: ob2h
  │    └─ MemoryProvider-плагин ($HERMES_HOME/plugins/ob2h/, Python, stdlib-only)
  │         ├─ prefetch()   → JSON-RPC → ob2h serve → гибридный поиск → <agent_memory>
  │         ├─ sync_turn()  → JSON-RPC → session_log (каждый ход, автоматически)
  │         ├─ on_session_end() → JSON-RPC → session_ingest (полная транскрипта)
  │         └─ get_tool_schemas() → graph_*/dream_*/knowledge_extract как инструменты
  │              └─ subprocess: ob2h.exe serve (единственный процесс, владеет БД и автодримом)
  └─ mcp_servers.ob2h — удалён владельцем вручную (опционально; Mode B допускает оба)

Синхронизация PC ↔ VPS (файловые бандлы, без сетевых сервисов):

PC ob2h ── sync export → bundle.jsonl.gz ── ssh/scp ──→ VPS: ob2h sync import
VPS ob2h ── sync export → bundle.jsonl.gz ──← ssh/scp ──  PC: ob2h sync import
(LWW по updated_at, tie-break по приоритету origin; tombstones; идемпотентно)
```

---

## 3. Фаза 7 — MemoryProvider-плагин (детерминированный поток в обе стороны)

Оценка: 2–3 дня. Код плагина живёт в репо в `plugin/ob2h/` и деплоится командой
`ob2h plugin install` (копия в `$HERMES_HOME/plugins/ob2h/`).

### 7.1 Что делает плагин: маппинг lifecycle → ob2h

| Хук Hermes | Действие плагина | Инструмент/механика ob2h |
|---|---|---|
| `is_available()` | Проверить, что найден бинарник ob2h и `--version` отвечает | `ob2h --version` (новый флаг, тривиален) |
| `initialize(session_id, hermes_home, platform, agent_context…)` | Спавнить `ob2h serve` (если ещё не жив), MCP-handshake `initialize` + `tools/list`; **не-primary контексты (cron/subagent) → write-through выключен** (иначе сисколлы cron-агента замусорят память — так делает Honcho) | JSON-RPC stdio |
| `system_prompt_block()` | Статический текст: «Долговременная память подключена; релевантное всплывает блоками `<agent_memory>`; для явных операций есть инструменты ob2h_*» | — |
| `queue_prefetch(query)` | Фоновый поток: `memory_search {query, mode: hybrid, limit: 8}` + `memory_context {query}`; кэш на один ход | существующие инструменты, без LLM, миллисекунды |
| `prefetch(query)` | Вернуть кэш `queue_prefetch` (или `""`); формат — готовый `<agent_memory>`-блок, без markdown-заборов (Hermes их вырезает) | — |
| `recall_status()` | `RecallStatus(count=N, glyph="🧠")` — детерминированный индикатор в UI | — |
| `sync_turn(user, assistant, session_id)` | Очередь записи → `session_log {user_text, assistant_text, source: "hermes"}` (неблокирующе, батчить не нужно — локально дёшево) | существующий инструмент |
| `on_session_end(messages)` | `session_ingest` (новый, §7.3): полная транскрипта парами + маркер конца сессии | новый инструмент |
| `on_pre_compress(messages)` | Тот же `session_ingest` с `source: "pre_compress"` — спасти контент до сжатия контекста Hermes | новый инструмент |
| `on_memory_write(action, target, content)` | Если builtin-память всё же пишет — зеркалировать в `memory_save {source: "hermes-builtin"}` | существующий |
| `get_tool_schemas()` | Проксировать `tools/list` от ob2h: `memory_save/update/forget`, `knowledge_extract`, `graph_search/reason/stats`, `dream_run/status/log/restore`, `omnes_stats/backup`. **Не** экспортировать `session_log`/`session_ingest` (они автоматические) и `memory_search`/`memory_context` (замещены prefetch) | — |
| `handle_tool_call(name, args)` | Прозрачный форвард `tools/call` в subprocess, таймаут из конфига | — |
| `shutdown()` | Закрыть stdin subprocess, дождаться выхода ≤5с, убить | — |

### 7.2 Архитектура плагина

- **Python 3.10+, только stdlib** (`subprocess`, `json`, `threading`, `queue`, `pathlib`).
  Ноль `pip_dependencies` — плагин не может «не установиться» из-за зависимостей.
- **Один долгоживущий subprocess** `ob2h serve`, JSON-RPC 2.0 поверх stdio
  (клиент ≈150 строк: `initialize`, `notifications/initialized`, `tools/list`, `tools/call`;
  конкурентность сервера уже обеспечена — ping/долгие вызовы не блокируют друг друга,
  фикс 4877a39). Перезапуск subprocess при падении с backoff,health-check ping каждые 60с.
- **Поиск бинарника** (в порядке): env `OB2H_BIN` → `memory.ob2h_binary` из конфига Hermes
  (через `get_config_schema`/`save_config` плагин показывает поле в UI настроек) →
  `C:/Projects/omnesbot_for_hermes/target/release/ob2h.exe` → `ob2h` в PATH.
- **Ограничения производительности**: prefetch — жёсткий таймаут 1.5с (локальный поиск без
  LLM успевает; иначе вернуть `""`, т.к. prefetch вызывается синхронно перед ходом);
  sync_turn/on_session_end — fire-and-forget очередь с фоновым потоком.
- **Файлы**: `plugin/ob2h/__init__.py` (класс `Ob2hProvider(MemoryProvider)` +
  `register_memory_provider` не нужен — discovery ищет класс по `__init__.py`),
  `plugin/ob2h/_rpc.py` (JSON-RPC клиент), `plugin/ob2h/plugin.yaml` (name/version/description).

### 7.3 Изменения в ob2h (Rust), фаза 7

1. **`--version` флаг** в `main.rs` (clap), печатает `env!("CARGO_PKG_VERSION")`.
2. **Новый MCP-инструмент `session_ingest`** (19-й; в конец списка):
   `{messages: [{role: "user"|"assistant", content: str}], source?: str = "hermes",
     session_id?: str}`. Пишет в daily-лог `YYYY-MM-DD.jsonl` парами
   user/assistant (существующий формат `DailyLogEntry`), затем `maybe_consolidate`
   как у `session_log`. Ошибки — строкой `[Error] …` (правило AGENTS §6).
   Дедуп: если `session_id` уже инжестился полностью (kv-ключ
   `ingested:<session_id>:<hash(messages_len)>`) — вернуть счётчик без повторной записи
   (защита от двойного вызова session_end + pre_compress).
3. **Env `OB2H_BIN`** нигде в Rust не нужен (это плагин ищет бинарник) — не добавлять.
4. **CLI `ob2h plugin install [--hermes-home <dir>]`**: копирует `plugin/ob2h/` в
   `<hermes-home>/plugins/ob2h/` и **печатает** сниппет для ручной вставки в config.yaml:
   ```yaml
   memory:
     provider: ob2h
   ```
   Конфиг Hermes автоматически **не** правится (AGENTS §1; исключением был только
   `install` для mcp_servers по явной команде владельца — сохраняем это различие).
   `ob2h plugin uninstall` — удаляет каталог, напоминает убрать `memory.provider`.
   `ob2h plugin status` — установлен/не установлен, жив ли subprocess (по lock/ping логам).

### 7.4 Режимы подключения

- **Mode A (рекомендуемый)**: плагин + удалённый владельцем `mcp_servers.ob2h`.
  Один процесс ob2h, один AutoDream, инструменты приходят через плагин
  (`inject_memory_provider_tools` в Hermes сам добавит их агенту).
- **Mode B (переходный)**: плагин и `mcp_servers.ob2h` одновременно. Работает: WAL-режим
  допускает два процесса, автодрим защищён файловым lock, случайные конфликты записи —
  через `busy_timeout` (проверить, что он выставлен в `db/mod.rs`; если нет — добавить
  `PRAGMA busy_timeout=5000`, это безопасно и для v0.7.1-клиентов). В скилле описать как
  временную конфигурацию.
- **Mode 0 (фолбэк)**: без плагина — поведение ровно как v0.7.1 (MCP-only). Полностью
  поддерживаем; скилл ведёт себя соответственно.

### 7.5 Тесты фазы 7

- Rust: `session_ingest` пишет пары и триггерит консолидацию (FakeLLM); повторный ingest
  того же `session_id` — no-op; снапшот-тест `tools/list` (имена/схемы 18 старых
  инструментов неизменны, `session_ingest` в конце).
- Python (плагин, `plugin/tests/test_rpc.py`, запуск `python -m unittest`): клиент
  против фейк-JSON-RPC-сервера (тред с stdin/stdout пайпами) — handshake, tools/call,
  таймаут, рестарт после «смерти» сервера.
- Интеграционный: плагин против реального `ob2h serve` во временном `OB2H_DATA_DIR` —
  `sync_turn` → в daily-логе появилась запись; `prefetch` после `memory_save` возвращает блок.
- Живой e2e (владелец, вручную): включить `memory.provider: ob2h`, перезапустить Hermes,
  диалог без упоминания памяти → в `data/workspace/daily/` растёт лог, в ответах виден
  индикатор 🧠, автодрим срабатывает по гейтам.

### 7.6 DoD фазы 7

Диалог Hermes без единого явного упоминания памяти: (1) автоматически пишется в daily-лог
каждый ход; (2) релевантные факты инжектятся перед ходом (индикатор recall); (3) завершение
сессии даёт полную транскрипту для dream-extract; (4) при выключенном плагине всё работает
как v0.7.1; (5) тесты Rust+Python зелёные, `cargo clippy` чист.

---

## 4. Фаза 8 — Синхронизация PC ↔ VPS

Оценка: 3–4 дня. Транспорт — **файлы поверх SSH** (никаких сетевых сервисов в ob2h,
ADR-1/ADR-7 не нарушаются; исходящий трафик за пределы машины — осознанное решение
владельца, фиксируется ADR-9 в PLAN.md §6).

### 8.1 Миграция схемы M2 (аддитивная, `db/schema.rs`)

Стабильные идентичности строк уже есть: `memories.key`, `graph_nodes.node_id`
(SHA256-дедуп), рёбра — `(source.node_id, target.node_id, label)`. Новые колонки:

```sql
-- MIGRATION_V2 (только ALTER TABLE ... ADD COLUMN с DEFAULT, + одна новая таблица)
ALTER TABLE memories     ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE memories     ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_nodes  ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE graph_nodes  ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_edges  ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE graph_edges  ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_edges  ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';
-- backfill: origin='' трактуем как "unknown/локальный", updated_at рёбер := created_at

CREATE TABLE IF NOT EXISTS sync_state (
  peer TEXT PRIMARY KEY,            -- 'vps' | 'pc' | ...
  last_export_at TEXT,              -- watermark updated_at последнего экспорта
  last_import_at TEXT,
  applied_bundles TEXT NOT NULL DEFAULT '[]'  -- JSON-массив bundle_id
);
```

- Перед применением M2 — копия БД в `data/backups/pre-v08-<UTC>.db` (код в `migrate()`).
- `SCHEMA_VERSION = 2`; логика `if current_version < 2 { … }` по образцу v1.
- Обновить `memory_forget`/`purge_weak`: физический DELETE → пометка `deleted_at`
  (tombstone), физическая чистка — только в maintenance автодрима старше
  `RETENTION_DAYS*2` (чтобы tombstone успел синхронизироваться). `memory_search`
  и `build_context` фильтруют `deleted_at IS NULL`. Семантика инструментов не меняется.
- `memory_relations`: не синхронизируем в 0.8 (см. §8.4).

### 8.2 Формат бандла и команды

Бандл — `data/sync/out/<bundle_id>.jsonl.gz`, где `bundle_id = <origin>-<UTC>-<short-hash>`:

```
строка 1 (header): {"type":"bundle", "bundle_id":…, "origin":"pc", "created_at":…,
                    "from":"…", "to":"…", "counts":{…}, "sha256":"…"}
строки 2..n: {"type":"mem"|"node"|"edge"|"tomb", "id":"<key|node_id|src|tgt|label>",
              "updated_at":…, "origin":…, "row":{ …полная строка таблицы… }}
```

- `ob2h sync export --peer <name>`: выбрать строки четырёх таблиц с
  `updated_at > sync_state.last_export_at` (плюс tombstones), упаковать, обновить
  watermark **после** успешной записи файла; embedding кладём в бандл как есть
  (hex) — модель у сторон одна (локальный Candle MiniLM, детерминирована), re-embed
  на приёме — только если вектора нет.
- `ob2h sync import <file>`: распаковать во временную таблицу, применить одной
  транзакцией UPSERT-ами; правило конфликтов — **LWW**: побеждает большая `updated_at`,
  при равенстве — `origin` выше по приоритету из конфига пира; идемпотентность по
  `applied_bundles` (повторный импорт того же файла — no-op) и по идентичностям строк;
  FTS-триггеры обновятся сами; в конце — `applied_bundles += bundle_id`,
  `last_import_at = now`. Перед первым применением незнакомого bundle_id — авто-бэкап
  (используя существующий `backup::snapshot`, 1 копия, без ротации).
- `ob2h sync status`: по каждому пиру — watermark, размер последнего бандла, счётчик
  применённых, возраст последнего успешного обмена.

### 8.3 Конфиг пиров и транспорт

`data/sync/peers.toml` (не в git, содержит хосты):

```toml
[node]            # собственная идентичность
origin = "pc"     # попадает в строки и в tie-break; у второй машины — "vps"
priority = ["vps", "pc"]   # порядок tie-break LWW

[peer.vps]
method  = "ssh"              # ssh | local | manual
host    = "user@vps-host"
remote  = "/srv/ob2h/sync/incoming"   # куда push'им свои бандлы
pull    = "/srv/ob2h/sync/outgoing"   # откуда забираем чужие

[peer.vps.schedule]
after_dream = true           # фаза автодрима после дрима делает push+pull
```

- `ob2h sync push --peer vps`: `scp` свежего бандла в `remote`; на VPS бандл ждёт
  местного импорта. Реализация — вызов системного `ssh`/`scp` (Windows OpenSSH есть
  из коробки), ключи — обычный `~/.ssh`; никаких SSH-библиотек в Cargo.
- `ob2h sync pull --peer vps`: `scp` чужих ещё не применённых бандлов из `pull` →
  локальный `import`.
- `method = "manual"`: ob2h только складывает/читает бандлы в `data/sync/`, перенос —
  руками/Syncthing/git-репо (описать в доке; для git-варианта — приватный репо
  обязательно). **Категорически не синхронизировать файловой синхронизацией живую
  `ob2h.db`** (WAL двух процессов = порча) — только папку бандлов.
- Шифрование покомпонентно: трафик уже внутри SSH; для `manual`-метода — опционально
  `age` (внешний бинарник) в 0.9, в 0.8 не тащим.
- Расписание на VPS: systemd timer (юниты положить в `scripts/vps/ob2h-sync.timer`
  + `.service`, `OnCalendar=*-*-* 04:30` после дрима) либо cron; на PC — фаза автодрима
  (`after_dream`) и/или Task Scheduler (скрипт `scripts/pc/sync-task.ps1`).

### 8.4 Что НЕ синхронизируем в 0.8 (осознанные границы)

- **Workspace MD (MEMORY/SOUL/USER.md), history.jsonl, daily-логи** — это локальные
  *представления*; после мержа БД дрим каждой стороны перегенерирует их сам.
  Синк файлов породил бы конфликтные правки текста без честного мержа.
- **documents/chunks** — большие и ре-ингестируемы (`knowledge_extract` по тому же
  файлу); в бэклог на 0.9, если понадобится.
- **memory_relations, dream_runs** — служебные/производные; отношения между фактами
  заново выведет дрим-консолидация.
- **Hermes `state.db`** — принадлежит Hermes, ob2h его не трогает (§1 AGENTS).

### 8.5 Тесты фазы 8

1. Миграция: копия БД, созданной «v0.7.1-кодом» (фикстура: применение только M1) →
   M2 → все строки получили origin/deleted_at, старые инструменты работают.
2. Roundtrip: две чистые БД (pc/vps) → взаимные save/edit → export/import обеих сторон →
   множества memories и графов идентичны (по key/node_id), включая embeddings.
3. LWW: одна строка правится на обеих машинах с разными `updated_at` → побеждает новая;
   равные → приоритет `origin` из `priority`.
4. Tombstone: forget на PC → импорт на VPS → строка скрыта из поиска на VPS.
5. Идемпотентность: повторный import того же бандла — no-op (счётчики не растут).
6. Даунгрейт: после M2 старые INSERT/SELECT (по именованным колонкам) работают —
   тест пишет строку «v0.7.1-стилем» (`INSERT INTO memories (key, content, …)` без
   новых колонок) и читает её.
7. Протокол: битый/обрезанный/чужой-origin бандл → `[Error] …`, транзакция откатана,
   watermark не двинулся.

### 8.6 DoD фазы 8

На двух машинах (или двух изолированных `OB2H_DATA_DIR` с `method=manual`):
факт, сохранённый на PC, после `push+pull` находится поиском на VPS и наоборот;
удаление реплицируется tombstone; повторный обмен — no-op; при отсутствии peers-конфига
поведение неотличимо от v0.7.1; авто-бэкап перед первым импортом бандла создаётся.

---

## 5. Фаза 9 — скилл, документация, релиз

Оценка: 1 день.

1. **Скилл в репо**: исходник живёт в `skills/ob2h/SKILL.md` (+ `references/`),
   деплой — `ob2h skill install` (копия в `$HERMES_HOME/skills/devops/ob2h/`).
   Новая версия скилла (уже написана, см. ниже) описывает: режимы 0/A/B, поведение
   «память всегда включена», питфоллы v0.7.1 (сохранены), команды sync.
   Стары hash-ссылки в `references/` проверить.
2. **docs/HERMES_INTEGRATION.md** — переписать под Rust-эру: сниппет `ob2h.exe serve`,
   режимы 0/A/B, установка плагина, sync-инструкция PC↔VPS, миграция с 0.7.1
   (замена бинарника + первый запуск сам мигрирует с бэкапом).
3. **AGENTS.md / CLAUDE.md** — актуализировать структуру репо (Rust-модули, `plugin/`,
   `skills/`, `data/sync/`), правила Python-части плагина (stdlib-only), правило
   «конфиг Hermes не правится автоматически, кроме явного install».
4. **CHANGELOG.md**: догнать запись 0.7.1 (перенос на Rust, инсталлятор), затем 0.8.0
   (Added: плагин, session_ingest, sync\*, plugin/skill install; Changed: ничего
   breaking; Fixed: busy_timeout, tombstone-семантика forget).
5. **README.md**: раздел «Плагин памяти» и «Синхронизация двух машин», версия 0.8.0.
6. **Релиз**: `cargo build --release` + архивы win/linux (процесс уже отлажен),
   тег не обязателен (локальный проект), версия в `Cargo.toml` = 0.8.0.

---

## 6. Порядок работ и зависимости

```
7.1 --version флаг ──┐
7.2 session_ingest ──┼─ 7.3 плагин (plugin/ob2h/) ── 7.4 plugin install ── 7.5 тесты ─ 7.6 e2e владельца
                     │
8.1 M2 миграция ── 8.2 export/import ── 8.3 transport+peers ── 8.4 автодрим-фаза ── 8.5 тесты
                     │
9.1 скилл ── 9.2 docs ── 9.3 AGENTS ── 9.4 CHANGELOG ── 9.5 README ── 9.6 релиз 0.8.0
```

Фазы 7 и 8 независимы по коду (кроме общей миграции M2, которая нужна только фазе 8),
но порядок 7→8→9 сохранять: плагин даёт немедленную пользу, синк без плагина на VPS
бесполезен (там нечего экспортировать без автозахвата).

## 7. Риски

| Риск | Митигация |
|---|---|
| API MemoryProvider изменится в обновлениях Hermes | Плагин использует только стабильные базовые хуки (initialize/prefetch/sync_turn/get_tool_schemas); при поломке — Mode 0 продолжает работать; в скилле диагностика `hermes` CLI |
| Двойной процесс (Mode B) дерётся за БД | WAL + `busy_timeout=5000`; автодрим за файловым lock; рекомендация Mode A в доке |
| Миграция M2 на большой живой БД | Только ADD COLUMN (мгновенно в SQLite) + один CREATE TABLE; бэкап-копия перед применением |
| SSH-ключи/хост недоступны с PC | `method=manual` (файлы) и git-вариант как фолбэк; sync-ошибки не роняют автодрим (best-effort фаза) |
| Расход LLM на dream-extract выросшего потока сессий | Существующие гейты автодрима; `session_ingest` дедуп по session_id; лимиты чанков уже в конфиге |
| Приватность: бандлы покидают машину | Только SSH на свою VPS / приватный git; ADR-9; никаких третьих сторон; шифрование — бэклог 0.9 |

## 8. Записи в журнал (внести в PLAN.md §6 при старте фаз)

- **ADR-9**: синхронизация — файловые gzip-бандлы поверх SSH/manual; ob2h не открывает
  сетевых портов и не получает сетевой зависимости (ADR-1/7 сохраняются).
- **ADR-10**: интеграция с Hermes — MemoryProvider-плагин (Python, stdlib) поверх
  долгоживущего `ob2h serve`; MCP-режим v0.7.1 остаётся поддержанным фолбэком.
- **ADR-11**: `memory_forget`/purge переходят на tombstones (LWW-совместимость);
  физическое удаление — только отложенное, в maintenance.
