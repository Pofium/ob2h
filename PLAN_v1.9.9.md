# План разработки OB2H v1.9.9: Memory Lifecycle Pack — уроки, слоты, хронология, провенанс и аудит памяти

> **Версия плана:** 1.9.9 (последняя точка линии 1.x — нумерация по указанию владельца; мажорный `PLAN_v2.0.md` остаётся отдельным черновиком)
> **Статус:** черновик к обсуждению и утверждению
> **Сформирован:** 2026-11-14
> **Источник:** анализ репозитория [rohitg00/agentmemory](https://github.com/rohitg00/agentmemory) v0.9.31 (TypeScript/Node, Apache-2.0). Анализ по исходникам: `src/functions/*` (уроки/слоты/verify/консолидация), `src/state/*` (KV-схема, гибридный поиск, стеммер/синонимы/CJK), `src/mcp/tools-registry.ts` (54 инструмента), `integrations/hermes/` (нативный провайдер), README/бенчмарки (LongMemEval-S).
> **Базовое состояние:** v1.9.2 — 41 MCP-инструмент, схема M10, Secrets Vault (M9), три эшелона LLM, E2EE-синк PC↔VPS, самообновление, Hermes/DSH-плагины, Ralph Knowledge Layer.
> **Принцип совместимости:**
> - Контракт MCP расширяется **аддитивно** (41 → 55 инструментов), существующие сигнатуры и семантика не меняются; запись в `CHANGELOG.md` обязательна.
> - Схема БД — только новая миграция **M11** (`schema_version` 10 → 11: `CREATE TABLE` + FTS5-индекс, без изменения существующих таблиц), по образцу M9/M10.
> - Правило **«никаких автоудалений»** (ADR-11) сохраняется: затухание уроков даёт только *кандидата* на забывание, физическое/tombstone-удаление — по явной команде.
> - Сеть/порты не затрагиваются (ADR-1/ADR-7): stdio MCP остаётся единственной поверхностью; никакого daemon/REST/viewer.
> - Синк расширяется только на `lessons` и `slots` (дельта-бандлы, поле-уровневый merge как у memories); `audit_log` — локальный журнал, в бандлы не входит.

---

## 1. Мотивация

Память OB2H сегодня — «плоский» конвейер: факт (`memories`) + trust/feedback + связи (`memory_links`) + ночной дрим. Сравнение с agentmemory (см. §3) выявило **пять зрелых подсистем жизненного цикла памяти**, которых у OB2H нет, при том что все они реализуемы в текущих архитектурных границах (SQLite, Rust, stdio MCP, без новых сервисов):

| Пробел | Как в OB2H сегодня | Как в agentmemory v0.9.31 | Что делаем (фаза) |
|---|---|---|---|
| **Уроки** (что сработало/что не надо повторять) | нет выделенной сущности; такие факты лежат в `memories` вперемешку с остальными, без своей механики уверенности | `lessons`: content/context, confidence 0..1, reinforce при повторе, недельный decay, BM25-индекс, recall = confidence×relevance×recency | Ф64 |
| **Слоты рабочего контекста** | свободные MD-файлы workspace (MEMORY/SOUL/USER) пишет дрим; агент в ходе сессии не имеет бюджетированного буфера «на сейчас» | `slots`: pinned-единицы с жёстким sizeLimit, scope project/global, append/replace с 413-семантикой, инъекция pinned в контекст | Ф65 |
| **Хронология и рекап** | daily-логи есть, но инструментов «что происходило вокруг даты X» и «сведи сессию Y» нет | `memory_timeline` (окно наблюдений вокруг якоря), on-demand рекап/кристаллизация цепочек, инкрементальные summaries (пропуск неизменённого префикса) | Ф66 |
| **Провенанс фактов** | meta (source/author/session) пишется, но нет инструмента «откуда взялся факт» | `memory_verify`: трассировка от факта к исходным наблюдениям с цитатами и confidence | Ф67 |
| **Аудит операций памяти** | только runtime-логи (неструктурированные, ротируются) | `audit ledger`: структурированная запись операций (сохранение/удаление/слоты/уроки) с actor и reason; governance-delete с причиной | Ф68 |

Плюс два сквозных улучшения по образцу agentmemory: **скрабинг секретов в конвейерах захвата** (Ф69.1) и **bench-метрики recall-all@k / any-hit@k** в стиле LongMemEval (Ф69.2) — у agentmemory это измерено публично (96.2% any-hit@5 / 84.7% recall-all@5 для keyless BM25 на LongMemEval-S), у OB2H bench пока считает только recall@k/MRR по одному релевантному документу на кейс.

Заказ владельца (2026-11-14): *«Проанализируй программу и репозитории [agentmemory]. Сделай новую спецификацию»*; нумерация 1.9.9 — по прямому указанию.

---

## 2. Термины

| Термин | Определение |
|---|---|
| Урок (lesson) | краткое утверждение «что делать / чего избегать / когда применимо», живущее с показателем уверенности (confidence 0..1) и контекстом применимости; дедуп по нормализованному тексту |
| Reinforce | укрепление урока: повторное сохранение того же контента повышает confidence по формуле насыщения и обновляет `last_reinforced_at` |
| Decay | затухание: раз в неделю в дриме confidence снижается на `decay_rate × недели_покоя`; пол 0.05; удалений нет |
| Слот (slot) | редактируемая агентом единица рабочего контекста с жёстким лимитом размера (`size_limit`), признаком `pinned` и скоупом `project`/`global` |
| Пин слота | включение содержимого непустого слота в блок `<agent_slots>` контекста (`memory_context`); не путать с PIN Secrets Vault |
| Якорь (anchor) | точка хронологии: ISO-дата либо текст, найденный FTS-поиском по daily-логам |
| Провенанс | цепочка цитат от факта `memories` к исходным записям сессий/daily-логов |
| Аудит-леджер | append-only таблица операций над памятью/слотами/уроками/секретами с actor, action, reason |

---

## 3. Анализ agentmemory (внешний as-is)

### 3.1 Архитектура

- **Форма:** локальный daemon на встроенном движке iii (Node 20+), REST API `:3111`, веб-viewer `:3113` (SSE, replay-плеер), MCP-сервер (54 инструмента) поверх daemon либо standalone. Хранение — SQLite-подобный KV (`state_store.db`, префиксы `mem:*` в `state/schema.ts`), **0 внешних СУБД**.
- **Захват:** 12 lifecycle-хуков Claude Code-формата (session start/end, prompt-submit, pre/post-tool-use, stop, pre-compact, subagent-*, task-completed, notification, post-commit, post-tool-failure) + плагины для ~25 агентов; capture-inbox, дедуп по event-id, офлайн-спул (`capture/spool.ts`) с реплеем, скрабинг секретов при записи.
- **Модель данных:** sessions → observations (сжатые события сессии с confidence) → memories (устойчивые факты с version/isLatest/supersedes, типами pattern/preference/architecture/bug/workflow/fact) → summaries (инкрементальные) + lessons + crystals + insights + граф (nodes/edges/temporal) + слоты + actions/leases/сигналы (мультиагентная оркестрация).
- **Поиск:** BM25 без ключа (keyless) как дефолт; локальные эмбеддинги `Xenova/all-MiniLM-L6-v2` опционально; `memory_smart_search` фузит структурные графовые совпадения; стеммер, синонимы, CJK-сегментер, query expansion, реранкер; два слоя цели (memory vs observation).
- **LLM-конвейеры:** сжатие наблюдений, консолидация (4 уровня working → episodic → semantic → procedural), crystallize (дайджесты завершённых цепочек действий), reflect (инсайты по кластерам графа) — всё opt-in через провайдера (openai/anthropic/gemini/…, circuit breaker + fallback chain).
- **Бенчмарк:** LongMemEval-S на BM25-компоненте: any-hit@5 **96.2%**, strict recall-all@5 **84.7%** (измерено 2026-10-28, 500 вопросов).

### 3.2 Уроки (src/functions/lessons.ts) — семантика, которую переносим

- Дедуп: `id = "lsn_" + sha256(trim(content).toLowerCase())[0..16]` — повторное сохранение того же контента **укрепляет**, а не дублирует: `reinforcements++`, `confidence = min(1.0, conf + 0.1·(1−conf))`, `last_reinforced_at = now`; опционально дозаполняется `context`, `source_ids` мержится (кап 50).
- Recall: BM25-индекс по урокам (полнотекстовый релеванс нормируется на топ-хит) × `confidence` × recency-буст `1/(1+дней·0.01)`; `min_confidence` дефолт 0.1; проектный фильтр; сортировка score desc, тай-брейк по id.
- Decay-sweep (недельный): для уроков «в покое» ≥1 недели `conf −= decay_rate(0.05)·недель`, пол 0.05; у agentmemory урок с `conf ≤ 0.1 && reinforcements == 0` **soft-deleted автоматически** — у нас это место меняется (ADR-11), см. §3.5.
- Источник урока: `manual | crystal | consolidation` (+`dream` у нас); у урока есть `source_ids` — мост к провенансу.

### 3.3 Слоты (src/functions/slots.ts) — семантика, которую переносим

- Валидация label `[a-z][a-z0-9_]{0,63}`; `size_limit` 1..20000 (дефолт 2000); `pinned` дефолт true; scope `project` (дефолт) | `global`; `read_only` для seeded.
- Чтение с затенением: project-слот затеняет global с тем же label; запись — в свой скоуп, дубликат в своём скоупе запрещён.
- `append` при переполнении → отказ с текущим размером и подсказкой «скompактируйте через replace» (413-семантика); `replace` проверяет лимит; операции под keyed-lock; всё пишется в аудит.
- Seed-дефолты (8 шт.: persona, user_preferences, tool_guidelines, project_context, guidance, pending_items, session_patterns, self_notes) — у нас адаптируются, чтобы не дублировать SOUL/USER.md дрима (см. §5.3).
- Инъекция в контекст: непустые pinned слоты, сортировка по label, рендер `## label` + содержимое.
- `slot-reflect` (env-флаг): авто-обновление pending_items/session_patterns/project_context из последних наблюдений сессии — у нас переносится в дрим за флагом (Ф66.4).

### 3.4 Провенанс (src/functions/verify.ts) — семантика, которую переносим

- `verify(id)`: если id — memory → по `sourceObservationIds` (+hint по `sessionIds`) собирает цитаты (наблюдение: title/type/confidence/timestamp/sessionId + сессия: project/status) и `citationCount`; если id — observation → возвращает саму запись с сессией; иначе честный `not found`. У нас наблюдениями служат daily-записи/`session_ingest`, поэтому провенанс = трассировка `memories.meta` + `source_ids` → записи daily-логов (Ф67).

### 3.5 Интеграция с Hermes (integrations/hermes) — сравнение с нашим MemoryProvider

agentmemory подключается к Hermes как native provider (prefetch, sync_turn, on_pre_compress, on_session_switch/end, on_memory_write, авторская скоупность per-turn, изоляция профилей, креды через `agent.secret_scope.get_secret`). **Наш MemoryProvider-плагин (`plugin/ob2h`) функционально эквивалентен** по prefetch/capture/авторам; отличия agentmemory, которые мы не копируем: HTTP-бэкенд с bearer-креденшелами (у нас — локальный процесс `ob2h serve`), ownership-файл для эволюции записей (у нас LWW-синк), MCP-мост опционально (у нас MCP — основной контракт). Вывод: интеграционный слой OB2H менять не нужно — все новинки v1.9.9 доезжают до Hermes/DSH автоматически через `memory_context` и MCP-контракт.

### 3.6 Что принято / что отклонено

| Идея agentmemory | Вердикт | Обоснование |
|---|---|---|
| Уроки с confidence/reinforce/decay | **Принято** (Ф64) | Зрелая механика, совместима с trust-философией OB2H |
| Слоты с sizeLimit/pinned/scope | **Принято** (Ф65) | Закрывает реальный пробел — агентский буфер без бюджета сейчас отсутствует |
| Timeline вокруг якоря | **Принято** (Ф66) | Daily-логи уже есть; нужен только read-инструмент |
| On-demand рекап сессии (аналог crystallize) | **Принято, упрощённо** (Ф66) | LLM-дайджест сессии + сохранение факта с source_ids; полноценные crystals/actions не нужны |
| Инкрементальные summaries | **Принято** (Ф66.3) | Оптимизация консолидатора: пропуск неизменённого префикса |
| memory_verify (провенанс) | **Принято** (Ф67) | Дёшево на наших meta; повышает доверие к фактам |
| Аудит-леджер + governance-delete с reason | **Принято с адаптацией** (Ф68) | Отдельный инструмент не нужен — reason встраивается в существующий `memory_forget`; audit — общий журнал |
| Скрабинг секретов при захвате | **Принято** (Ф69.1) | Строится на нашем Secrets Vault (имена записей известны без unlock) |
| Bench-метрики recall-all@k/any-hit@k | **Принято** (Ф69.2) | Прямое улучшение золотого набора; гейт ADR-13 становится строже |
| Авто-soft-delete урока при decay | **Отклонено** | ADR-11: candidate_for_forget — да, удаление — только явная команда |
| Daemon/REST/viewer/порты 3111/3113 | **Отклонено** | ADR-7: ob2h не открывает портов; stdio MCP |
| Node/iii-engine runtime | **Отклонено** | Ядро Rust, ноль новых runtime (ADR-1..8) |
| Actions/leases/frontier/next/signals/sentinels/sketches/checkpoints/routines | **Отклонено** | Мультиагентная оркестрация/мультиарендность вырезаны осознанно (AGENTS.md §3) |
| team_share/team_feed/mesh_sync | **Отклонено** | Роль выполняет собственный E2EE-синк PC↔VPS (v1.6/v1.8) |
| vision_search (CLIP), image embeddings | **Отклонено** | Тяжёлые мультимодальные модели вне границ; бэклог |
| import-jsonl (транскрипты Claude Code) | **Бэклог** | Захват у нас через плагины; импорт чужих транскриптов — отдельное решение |
| obsidian_export | **Бэклог** | Мило, но не мешает ничему; после Ф64–Ф69 |
| claude_bridge (зеркало MEMORY.md) | **Бэклог с оговоркой** | Конфликтует с правом дрима писать MEMORY.md; только двусторонний merge по явной команде |
| Реранкер (ort), стеммер/синонимы/CJK | **Бэклог/отложено** | Реранкер уже отложен (ADR-14, Ф25.3 v1.3); FTS5 trigram в OB2H уже покрывает инфлексию |
| 12 capture-хуков | **Отклонено** | Путь захвата OB2H — MemoryProvider/cordis-плагины; дублировать хук-слой не нужно |

---

## 4. As-is: существующая база OB2H (что уже есть)

| Компонент | Реализация | Роль / переиспользуем |
|---|---|---|
| SQLite `data/ob2h.db` | WAL, `schema_version` = 10 (M10 = FTS5-граф); миграции `src/db/schema.rs` | точка добавления M11 |
| MCP-сервер (stdio) | `src/mcp/`, **41 инструмент**, ошибки — строка `[Error] … [Подсказка]` | добавить 14 инструментов |
| Память | `memories` + FTS5 + эмбеддинги (Candle MiniLM 384d, int8 v2), trust/feedback, tombstones, `memory_links` (typed, PPR) | lessons/slots — рядом, не внутри: свои таблицы и сервисы |
| Daily-логи / захват | workspace daily-логи + `session_log`/`session_ingest` (дедуп по позиции, `meta.author`), плагины Hermes/DSH | источник для timeline/recap/провенанса |
| Дрим | `src/dream/` — ночная консолидация, ревизия trust, bench-гейт (ADR-13), курсоры в kv | сюда: decay-sweep уроков, опц. slot-reflect, инкрементальные summaries |
| Консолидатор | `src/consolidator/` — суммаризация сессий через `llm_client` | рекап сессии (on-demand) + инкрементальный префикс-курсор |
| Синк | `src/sync/` — дельта-бандлы, поле-уровневый merge + conflicts.jsonl, LWW | добавить lessons/slots в экспорт; audit_log — НЕ экспортируется |
| Secrets Vault | `src/secrets/`, M9 | имена записей — для скрабинга (Ф69.1) |
| Bench | `ob2h bench` (golden 36, recall@k/MRR), `data/bench/` | расширение метрик (Ф69.2), обратная совместимость golden-формата |
| Скилл/плагины | `skills/ob2h`, `plugin/ob2h`, `plugin/dsh-ob2h` | дополнить питфоллами про lessons/slots; код плагинов не меняется |

---

## 5. Целевая архитектура

```
┌──────────────────────── Hermes / DSH / любой MCP-клиент ────────────────────────┐
│  MCP (аддитивно, 41 → 55):                                                       │
│   lesson_save · lesson_recall · lesson_list · lesson_delete                      │
│   slot_list · slot_get · slot_create · slot_append · slot_replace · slot_delete  │
│   memory_timeline · session_recap · memory_verify · memory_audit                 │
└───────────────────────────────────┬─────────────────────────────────────────────┘
                                    │ stdio MCP
┌───────────────────────────────────▼─────────────────────────────────────────────┐
│                          ob2h (Rust, stdio MCP)                                  │
│  ┌──────────────────┐  ┌──────────────────┐  ┌────────────────────────────────┐ │
│  │ LifecycleService │  │ SlotsService     │  │ TimelineService / Recap        │ │
│  │ (src/lifecycle/  │  │ (src/lifecycle/  │  │ (read-only по daily-логам;     │ │
│  │  lessons.rs,     │  │  slots.rs)       │  │  LLM-рекап через llm_client)   │ │
│  │  audit.rs)       │  │                  │  │                                │ │
│  └────────┬─────────┘  └────────┬─────────┘  └───────────────┬────────────────┘ │
│           │   M11: lessons + lessons_fts (trigram, external content)            │
│           │   M11: slots (label PK, scope project|global)                        │
│           │   M11: audit_log (append-only)                                       │
│  ┌────────▼───────────────────────▼───────────────────────────────────────────┐ │
│  │ SQLite ob2h.db (WAL) — существующие таблицы не изменяются                  │ │
│  └────────────────────────────────────────────────────────────────────────────┘ │
│  Дрим: decay-sweep уроков (недельный шаг) · опц. slot-reflect (флаг) ·          │
│        инкрементальные summaries (курсор префикса сессии)                        │
│  Синк: lessons/slots — в дельта-бандлы (поле-уровневый merge); audit — нет       │
│  memory_context: блок <agent_slots> (pinned, бюджет OB2H_SLOTS_MAX_CHARS)        │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 5.1 Миграция M11 (эскиз DDL)

```sql
CREATE TABLE IF NOT EXISTS lessons (
    id TEXT PRIMARY KEY,              -- "lsn_" + sha256(norm)[0..16]
    content TEXT NOT NULL,
    context TEXT NOT NULL DEFAULT '',
    confidence REAL NOT NULL DEFAULT 0.5,
    reinforcements INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'manual',   -- manual|dream|consolidation|recap
    source_ids TEXT NOT NULL DEFAULT '[]',   -- JSON, cap 50
    project TEXT,
    tags TEXT NOT NULL DEFAULT '[]',         -- JSON
    decay_rate REAL NOT NULL DEFAULT 0.05,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_reinforced_at TEXT,
    last_decayed_at TEXT,
    deleted_at TEXT                          -- tombstone (ADR-11), не boolean
);
CREATE VIRTUAL TABLE IF NOT EXISTS lessons_fts USING fts5(
    content, context, content='lessons', content_rowid='rowid', tokenize='trigram'
);
-- триггеры ai/ad/au — как в M10 (graph_nodes_fts)

CREATE TABLE IF NOT EXISTS slots (
    label TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'project',   -- 'project' | 'global'
    project TEXT,                            -- NULL для global
    content TEXT NOT NULL DEFAULT '',
    size_limit INTEGER NOT NULL DEFAULT 2000, -- 1..20000
    description TEXT NOT NULL DEFAULT '',
    pinned INTEGER NOT NULL DEFAULT 1,
    read_only INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (label, scope)
);

CREATE TABLE IF NOT EXISTS audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    actor TEXT NOT NULL,                     -- agent|user|dream|system
    action TEXT NOT NULL,                    -- lesson_save|lesson_recall|lesson_delete|
                                             -- slot_*|memory_forget|memory_feedback|...|
                                             -- secrets_get|secrets_set|secrets_delete
    target_ids TEXT NOT NULL DEFAULT '[]',   -- JSON
    details TEXT NOT NULL DEFAULT '{}',      -- JSON: reason, before/after sizes и т.п.
    source_tool TEXT                         -- имя MCP-инструмента или CLI-команды
);
```

Ключевой отличие от agentmemory: PK слотов `(label, scope)` вместо двух KV-хранилищ — затенение реализуется запросом (`WHERE scope='project' AND project=?` → fallback `scope='global'`); у lessons tombstone `deleted_at` вместо `deleted: bool` — единообразие с memories и совместимость с LWW-синком.

### 5.2 Скоринг и формулы (зафиксировать до реализации, калибровка — только по bench, ADR-13)

- **Reinforce:** `conf' = min(1.0, conf + 0.1·(1 − conf))`; `reinforcements += 1`; обновить `last_reinforced_at`.
- **Decay (недельный шаг в дриме):** `conf' = max(0.05, conf − decay_rate · weeks_с_базлайна)`; базлайн = `last_decayed_at || last_reinforced_at || created_at`. Кандидат на забывание: `conf ≤ 0.10 && reinforcements == 0` → `category=candidate_for_forget`-пометка в отчёте дрима; **не удалять**.
- **Recall-скоринг:** `score = confidence × relevance × recency_boost`, где `relevance` — bm25-хит, нормированный на топ-хит; `recency_boost = 1 / (1 + days_since_reinforce · 0.01)`; сортировка по score desc, тай-брейк по id (как в Ф54 — детерминизм).
- **Бюджет слотов в контексте:** `OB2H_SLOTS_MAX_CHARS` (дефолт 1500); заполнение — по алфавиту label, усечение по границе целого слота (как бюджет prefetch по record-границам).

### 5.3 Разделение ролей: workspace-файлы vs слоты vs уроки (зафиксировать в HERMES_INTEGRATION)

| Слой | Кто пишет | Кто читает | Природа |
|---|---|---|---|
| `SOUL.md` / `USER.md` / `MEMORY.md` | дрим (ночью, с git-версионированием) | агент через workspace_read; человек | долгосрочный «характер» и сводки |
| Слоты | агент в ходе сессии (MCP) | инъекция `<agent_slots>` в `memory_context`; агент | рабочий буфер «на сейчас», жёсткий бюджет |
| Уроки | агент (explicit) + дрим/рекап (source) | `lesson_recall` перед решениями | процедурный опыт с уверенностью |

Seed-дефолты слотов OB2H (создаются лениво при первом `slot_list`/`memory_context`, `read_only=0`, адаптация восьми agentmemory-дефолтов, без дублирования SOUL/USER):

| label | scope | limit | назначение |
|---|---|---|---|
| `pending_items` | project | 2000 | незавершённые задачи, обещания |
| `session_guidance` | project | 1500 | фокус/риски на следующую сессию |
| `project_context` | project | 3000 | команды сборки/теста, конвенции репо |
| `user_rules` | global | 1500 | стойкие правила инструментария пользователя |
| `self_notes` | project | 1500 | гипотезы, тупики (pinned=false) |

---

## 6. Фазы

Нумерация продолжается после Ф63 (v1.9). Оценки — фокусные дни.

### Фаза 64 — Уроки (lessons): таблица, сервис, MCP, дрим-decay (оценка 1–1.5 дня)

- [ ] **64.1** M11: `lessons` + `lessons_fts` (trigram, external content, триггеры как M10) + `slots` + `audit_log` (все три таблицы одной миграцией, чтобы не гонять M11/M12/M13); `schema_version` 10 → 11; тест схемы.
- [ ] **64.2** `src/lifecycle/mod.rs` + `lessons.rs`: save с fingerprint-дедупом и reinforce, recall (FTS5 × confidence × recency, min_confidence, project-фильтр, детерминированный тай-брейк), list (по source/confidence/project), delete (tombstone). Все операции пишут в audit_log.
- [ ] **64.3** MCP: `lesson_save` (content, context?, confidence?, project?, tags?, source?) → `{action: created|strengthened}`; `lesson_recall` (query, project?, min_confidence=0.1, limit=10); `lesson_list` (project?, source?, min_confidence?, limit=50); `lesson_delete` (lesson_id). Ошибки — `[Error] … [Подсказка]`.
- [ ] **64.4** Дрим: недельный decay-sweep (курсор в kv: `lessons_decay_last_ts`), отчёт decayed/candidates в результате дрима; conf ≤ 0.10 && reinforcements == 0 → пометка candidate_for_forget, без удаления.
- [ ] **64.5** CLI `ob2h lessons list/decay`; тесты (без сети): дедуп/reinforce-математика, decay-пороги, recall-скоринг, tombstone.
- **DoD:** 4 инструмента в контракте; повторный save того же контента возвращает `strengthened` и не создаёт дубль; decay не удаляет ничего; `cargo test`/`clippy` зелёные.

### Фаза 65 — Слоты (slots): сервис, MCP, инъекция в контекст, синк (оценка 1–1.5 дня)

- [ ] **65.1** `src/lifecycle/slots.rs`: валидация label/limit/scope; затенение project→global; append/replace c 413-семантикой (ошибка с current_size/size_limit и подсказкой); keyed-лок на label; seed-дефолтов §5.3 (лениво).
- [ ] **65.2** MCP: `slot_list`, `slot_get`, `slot_create`, `slot_append`, `slot_replace`, `slot_delete` — контракты 1:1 по agentmemory (§3.3). Операции в audit_log.
- [ ] **65.3** `memory_context`: блок `<agent_slots>` после `<agent_memory>` — pinned+непустые, алфавитный порядок, бюджет `OB2H_SLOTS_MAX_CHARS=1500` по границе слота; слоты не попадают в векторный индекс (это контекст, а не факты — осознанно).
- [ ] **65.4** Синк: `lessons`/`slots` в дельта-бандлы и поле-уровневый merge (LWW по `updated_at`, конфликты — в conflicts.jsonl, `OB2H_SYNC_KEEP_LOSERS`); `audit_log` и `secrets` вне бандлов; `sync verify` учитывает новые таблицы.
- [ ] **65.5** Тесты: затенение, лимиты, бюджет инъекции, merge-конфликт слота на двух «машинах» (tmp-БД).
- **DoD:** 6 инструментов; append при переполнении даёт читаемую ошибку с подсказкой; pinned-слоты видны в `<agent_slots>`; синк переносит слот PC↔VPS без потерь.

### Фаза 66 — Хронология, рекап сессии, инкрементальные summaries (оценка 1–1.5 дня)

- [ ] **66.1** `memory_timeline(anchor, before=5, after=5, project?)`: якорь = ISO-дата либо текст (FTS-хит по daily-логам как якорь); окно записей до/после с sessionId/author/временем; read-only поверх workspace daily-логов и `session_ingest`-данных.
- [ ] **66.2** `session_recap(session_id?, save=?)`: дайджест последней/указанной сессии через `llm_client` (FakeLLM в тестах); `save=true` → факт в `memories` (type=recap, source=recap) + source_ids (ids daily-записей) → мост к провенансу; без сети в тестах.
- [ ] **66.3** Инкрементальные summaries в консолидаторе: курсор «просуммированного префикса» сессии в kv; повторный дрим пропускает неизменённый префикс (как incremental session summaries agentmemory).
- [ ] **66.4** Опционально (флаг `OB2H_DREAM_SLOTS`, дефолт off): dream-слот-рефлекс — переносит свежие TODO из daily-логов в `pending_items` (append, с уважением лимита) и счётчики паттернов в `session_guidance` — порт `slot-reflect` §3.3.
- **DoD:** timeline возвращает корректное окно вокруг даты и вокруг FTS-хита; recap с FakeLLM детерминирован; повторный дрим на неизменённой сессии не делает LLM-вызов (логируем skip).

### Фаза 67 — Провенанс фактов (оценка 0.5–1 день)

- [ ] **67.1** MCP `memory_verify(memory_id)`: по `memories.meta` (source/author/session_id) + source_ids (recap/lessons) собирает цитаты из daily-логов/сессий: id, timestamp, author, фрагмент (обрезка по budget), возвращает `citations`, `citation_count`; отсутствие метаданных — честный `provenance: none`, не ошибка.
- [ ] **67.2** Тесты: факт с сессионной метой → 1+ цитата; факт без меты → `provenance: none`; tombstone-факт → цитаты не теряются.
- **DoD:** 1 инструмент; verify отвечает за <50 мс на живой БД-копии (замер в отчёте фазы).

### Фаза 68 — Аудит-леджер (оценка 1 день)

- [ ] **68.1** `src/lifecycle/audit.rs`: единая точка `record_audit(actor, action, targets, details, tool)`; инструментирование существующих операций: `memory_forget` (+ новый опц. аргумент `reason` — governance-семантика, контракт аддитивен), `memory_feedback`, `memory_merge`, `lesson_*`, `slot_*`, `secrets_set/get/delete`, `workspace_write`.
- [ ] **68.2** MCP `memory_audit(operation?, limit=50)` — просмотр леджера (последние записи, фильтр по action); CLI `ob2h audit [--action …]`.
- [ ] **68.3** Ретеншн: записи старше 90 дней вычищаются в maintenance (id-шники, не контент); в бандлы синка не входит.
- **DoD:** 1 инструмент + reason у memory_forget; каждая операция из 68.1 оставляет запись; ротация не трогает свежие.

### Фаза 69 — Скрабинг секретов, bench-метрики recall-all/any-hit, доки, релиз 1.9.9 (оценка 1 день)

- [ ] **69.1** Редакция секретов в конвейерах: перед записью в daily/session_ingest и перед LLM-конвейерами (dream/consolidator/extractor) — маскирование по паттернам (`sk-…`, `ghp_…`, `Bearer …`, `://user:pass@`, `OB2H_[A-Z_]+=…`) и по именам записей Secrets Vault (имя известно без unlock); замена `[REDACTED:…]`; гейт `OB2H_REDACT=0`; тест-матрица паттернов.
- [ ] **69.2** `ob2h bench`: новые метрики **recall-all@k** (все релевантные источники кейса найдены в топ-k) и **any-hit@k** (хотя бы один) — golden-формат расширяется множеством релевантных id, старые кейсы (одно релевантное) читаются как раньше; вывод в отчёт и `bench/history.jsonl` (для гейта ADR-13 — пока наблюдение, порог не менять без данных).
- [ ] **69.3** Доки: README (пункт-новинка + счётчик инструментов), `CHANGELOG.md` `[1.9.9]` (контракт 41 → 55), `docs/HERMES_INTEGRATION.md` (§5.3 разделение ролей, блок `<agent_slots>`), `skills/ob2h/SKILL.md` + ZCode-вариант (питфоллы: когда lesson_save vs memory_save, лимиты слотов), `docs/SYNC.md` (lessons/slots в бандлах, audit — нет).
- [ ] **69.4** Релиз: версия 1.9.9, тег, GitHub Release (матрица без новых зависимостей — бинарник не растёт: новые модули = std + существующие crates).
- **DoD:** метрики считаются на старом и новом golden; скрабинг ловит тест-матрицу; доки/CHANGELOG/релиз оформлены по §8 AGENTS.md.

---

## 7. Совместимость

| Контур | Влияние | Правило |
|---|---|---|
| MCP-контракт | 41 → 55, аддитивно | сигнатуры старых не менять; `memory_forget` получает **опциональный** `reason`; запись в CHANGELOG |
| Схема | M11, только CREATE TABLE/INDEX/TRIGGER | менять существующие таблицы запрещено; `schema_version` 10 → 11 |
| Синк PC↔VPS | lessons/slots добавляются в дельта-бандлы | поле-уровневый merge, LWW `updated_at`, conflicts.jsonl; audit/secrets вне бандлов (ADR-21 преемственность) |
| Дрим | decay-sweep уроков; опц. slot-reflect; инкрементальные summaries | всё за флагами/курсорами, возобновляемо (stdio-процесс может умереть) |
| Плагины Hermes/DSH | **код не меняется** | слоты доезжают через `memory_context`; уроки — через MCP-контракт |
| Secrets Vault | изоляция сохраняется (ADR-21/22) | lessons/slots/audit не знают о значениях секретов; скрабинг — только по именам/паттернам |
| Bench-гейт (ADR-13) | recall@5/MRR не должны деградировать | новые метрики — наблюдение; порог отката не меняется в этой версии |

## 8. Риски

| Риск | Вероятность | Митигация |
|---|---|---|
| Слоты конфликтуют с ролью MEMORY.md/SOUL.md (агент пишет в оба) | средняя | §5.3 — разделение ролей в доках и скилле; слоты — «на сейчас», файлы — «долгий характер»; питфолл в SKILL.md |
| Разъезд слотов между машинами (append на обеих) | средняя | LWW + conflicts.jsonl; при конфликте слот целиком берётся у победителя, проигравший контент — в conflicts.jsonl для ручного склейки |
| FTS5 trigram по русскому тексту уроков | низкая | тот же tokenizer, что в M10 (уже работает на графе); проверка на живой копии БД в Ф64 |
| LLM-вызовы рекапа при отсутствии ключа | низкая | три эшелона LLM (v1.9.2); в тестах FakeLLM; при полном отсутствии модели — `[Error]` с подсказкой, не падение |
| Рост audit_log | низкая | ротация 90 дней в maintenance; запись — только метаданные операций |
| Миграция M11 на живой БД 900+ МБ | низкая | CREATE-only, как M9/M10; `ob2h backup quick` перед релизной инструкцией |

## 9. Оценка

| Фаза | Дней |
|---|---|
| Ф64 Уроки | 1–1.5 |
| Ф65 Слоты | 1–1.5 |
| Ф66 Хронология/рекап | 1–1.5 |
| Ф67 Провенанс | 0.5–1 |
| Ф68 Аудит | 1 |
| Ф69 Скрабинг/bench/доки/релиз | 1 |
| **Итого** | **5.5–7.5 фокусных дней** |

## 10. Общий DoD версии 1.9.9

1. `cargo test` и `cargo clippy --all-targets` зелёные; Python-тесты плагина не сломаны (контракт-тест актуализирован до 55 инструментов).
2. Контракт MCP 41 → 55 задокументирован в `CHANGELOG.md`; tools/list отсортирован, ошибки — `[Error] … [Подсказка]`.
3. `ob2h bench` без деградации recall@5/MRR (гейт ADR-13); новые метрики recall-all@5/any-hit@5 выводятся.
4. Синк: `sync verify` зелёный с lessons/slots в бандле; audit_log и secrets вне бандлов.
5. Никаких автоудалений: decay даёт только кандидатов; tombstone — по явной команде.
6. Доки: README / HERMES_INTEGRATION / SYNC.md / скиллы обновлены; разделение ролей §5.3 зафиксировано.
