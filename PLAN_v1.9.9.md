# План разработки OB2H v1.9.9: Memory Lifecycle Pack — уроки, слоты, хронология, провенанс и аудит памяти

> **Версия плана:** 1.9.9 (последняя точка линии 1.x — нумерация по указанию владельца; мажорный `PLAN_v2.0.md` остаётся отдельным черновиком)
> **Статус:** черновик к обсуждению и утверждению
> **Сформирован:** 2026-11-14; в тот же день дополнено фазами Ф70–Ф71 (визуальная часть; стек выбран владельцем — нативное окно egui)
> **Источник:** анализ репозитория [rohitg00/agentmemory](https://github.com/rohitg00/agentmemory) v0.9.31 (TypeScript/Node, Apache-2.0). Анализ по исходникам: `src/functions/*` (уроки/слоты/verify/консолидация), `src/state/*` (KV-схема, гибридный поиск, стеммер/синонимы/CJK), `src/mcp/tools-registry.ts` (54 инструмента), `integrations/hermes/` (нативный провайдер), README/бенчмарки (LongMemEval-S).
> **Базовое состояние:** v1.9.2 — 41 MCP-инструмент, схема M10, Secrets Vault (M9), три эшелона LLM, E2EE-синк PC↔VPS, самообновление, Hermes/DSH-плагины, Ralph Knowledge Layer.
> **Принцип совместимости:**
> - Контракт MCP расширяется **аддитивно** (41 → 56 инструментов), существующие сигнатуры и семантика не меняются; запись в `CHANGELOG.md` обязательна.
> - Схема БД — только новая миграция **M11** (`schema_version` 10 → 11: `CREATE TABLE` + FTS5-индекс, без изменения существующих таблиц), по образцу M9/M10.
> - Правило **«никаких автоудалений»** (ADR-11) сохраняется: затухание уроков даёт только *кандидата* на забывание, физическое/tombstone-удаление — по явной команде.
> - Сеть/порты не затрагиваются (ADR-1/ADR-7): stdio MCP остаётся единственной сетевой поверхностью; никакого daemon/REST; viewer — нативное окно без портов (Ф70–Ф71).
> - Синк расширяется только на `lessons` и `slots` (дельта-бандлы, поле-уровневый merge как у memories); `audit_log` — локальный журнал, в бандлы не входит.
> - **Viewer** — нативное окно egui под feature `gui` (стек и прецедент уже в репо — окно PIN, ADR-23); без портов и браузера; доступ к БД read-only без побочных эффектов (не трогает touch/trust/access). Решение владельца 2026-11-14: нативное окно вместо web-панели (web-форма — бэклог). Дополнение того же дня: вкладка «Секреты» — единственное исключение из read-only (только через переиспользуемый SecretsService + audit_log, §5.5); PIN — только локальный ввод в окне (ADR-18).

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
| Веб-форма viewer (браузер + SSE + реплей) | **Принято с адаптацией** (Ф70–Ф71) | Контент viewer (память/граф/сессии/реплей) переносится в нативное окно egui — без портов и браузера; стек уже в репо (feature `gui`, ADR-23); web-форма — бэклог |
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

### 5.4 Viewer — визуальная часть (нативное окно egui; решение владельца 2026-11-14)

- **Форма:** локальное окно того же бинарника — процесс `ob2h viewer` (eframe 0.37, feature `gui`); никакого HTTP/браузера/портов — ADR-7 не затрагивается. Headless-сборка (без `gui`) отвечает `[Error] … [Подсказка]`, не паникует (подход `catch_unwind` из ADR-23).
- **Жизненный цикл:** viewer — **процесс, отдельный от `ob2h serve`** — окно переживает завершение MCP-сессии агента. Детач-спавн (`CREATE_BREAKAWAY_FROM_JOB`/`DETACHED_PROCESS` на Windows) проверяется живой проверкой под Hermes/ZCode/DSH (kill-tree поведение агентов различается — Ф71.3).
- **Доступ к данным:** SQLite read-only (`SQLITE_OPEN_READ_ONLY`, WAL) параллельно с serve; **ни одной записи**: viewer не обновляет touch/access/trust/last_accessed (иначе искажение trust-телеметрии и bench-гейта ADR-13).
- **Single-instance:** lockfile `data/viewer.lock` (pid + проверка живости); повторный `viewer_open` → статус `already_running`, второго окна нет.
- **Табы:** Память (FTS-поиск; фильтры проект/категория/источник/min trust; детали с meta и `memory_links`) · Граф (force-layout топ-узлов по степени, cap `OB2H_VIEWER_GRAPH_MAX_NODES=300`, pan/zoom, цвет ребра по типу) · Сессии (daily-логи, ходы user/assistant, поиск — аналог реплея agentmemory) · Уроки/Слоты (данные появятся после Ф64/Ф65; до того — «нет данных») · **Секреты** (init/unlock/список/добавление — §5.5) · Статистика (счётчики, размер БД, schema_version, топ проектов/категорий; имена секретов — без значений).
- **UX:** тёмная/светлая тема (встроенная egui), авто-обновление 2 с + F5, RU/EN (`sys-locale`, `OB2H_UI_LANG` — переиспользование подхода ADR-23).

### 5.5 Секреты во viewer: PIN-доступ, добавление, инициализация (дополнение владельца 2026-11-14)

Вкладка «Секреты» — тот же конечный автомат состояний, что у SecretsService, в локальном окне:

| Состояние | UI | Действие |
|---|---|---|
| сейф не инициализирован | заглушка «Сейф не создан» + кнопка **«Начать сохранять»** | init: окно PIN (ввод дважды со сверкой) — то же, что `ob2h secrets init` (M9 kv: salt/verifier/kdf_params); повторный init на существующем сейфе запрещён с подсказкой |
| заблокирован | поле PIN (ровно 4 цифры, `Enter`, как в окне PIN ADR-23) | unlock: `K = Argon2id(pin, salt)` — только в памяти процесса viewer; на экране TTL-индикатор |
| разблокирован | список записей (имя, hint, обновлено, access_count) | добавить запись (имя+значение+hint, валидация имени как `secrets_set`); показать значение (по умолчанию маска `••••`, по кнопке — раскрытие, авто-скрытие через 15 с); копировать в буфер; удалить (с подтверждением); кнопка «Заблокировать» |

**Правила безопасности (фиксируем до реализации):**

1. **PIN никогда не покидает окно viewer** — локальный ввод в том же процессе (полное соответствие ADR-18: через MCP/LLM/чат PIN не проходит ни при каком сценарии).
2. **Только переиспользование SecretsService** (`src/secrets/service.rs`): крипто XChaCha20-Poly1305/Argon2id, verifier, TTL (дефолт 30 мин, `OB2H_SECRETS_TTL`), zeroize при lock/TTL/закрытии окна. Новой криптографической реализации нет.
3. **Исключение из read-only — единственное и точечное**: viewer пишет в БД только через SecretsService (таблица `secrets` + kv) и в `audit_log`; вкладки памяти/графа/сессий остаются строго read-only (правило Ф70 в силе).
4. **Аудит**: каждая операция вкладки → `audit_log` (`actor=user`, `source_tool=viewer`); в аудит пишутся только имена и размеры — **значения секретов не пишутся никуда** (ни в аудит, ни в логи, ни в daily).
5. **Независимые unlock-сессии**: viewer и `ob2h serve` держат K каждый в своей памяти; разблокировка во viewer не открывает MCP-инструменты `secrets_*` и наоборот; IPC-канал CLI не трогается. Параллельные писатели — WAL + `busy_timeout` (как два процесса ob2h в ADR-16/17).
6. Значение на экране по умолчанию замаскировано; авто-скрытие 15 с; копирование в буфер обмена — осознанное действие с предупреждением в подсказке.

---

## 6. Фазы

Нумерация продолжается после Ф63 (v1.9). Оценки — фокусные дни.

### Фаза 64 — Уроки (lessons): таблица, сервис, MCP, дрим-decay (оценка 1–1.5 дня)

- [x] **64.1** M11: `lessons` + `lessons_fts` (trigram, external content, триггеры как M10) + `slots` + `audit_log` (все три таблицы одной миграцией, чтобы не гонять M11/M12/M13); `schema_version` 10 → 11; тест схемы.
- [x] **64.2** `src/lifecycle/mod.rs` + `lessons.rs`: save с fingerprint-дедупом и reinforce, recall = FTS5 × confidence × recency (min_confidence, project-фильтр, детерминированный тай-брейк, LIKE-фолбэк), list (по source/confidence/project), delete (tombstone). Все операции пишут в audit_log.
- [x] **64.3** MCP: `lesson_save` (content, context?, confidence?, project?, tags?, source?) → `{action: created|strengthened}`; `lesson_recall` (query, project?, min_confidence=0.1, limit=10); `lesson_list` (project?, source?, min_confidence=0, limit=50); `lesson_delete` (lesson_id). Ошибки — `[Error] … [Подсказка]`; аннотации: recall/list readOnly, delete destructive.
- [x] **64.4** Дрим: недельный decay-sweep в autodream-регламенте после purge (курсор kv `lessons_decay_last_ts`), conf ≤ 0.10 && reinforcements == 0 → кандидат в отчёте, без удаления.
- [x] **64.5** CLI `ob2h lessons list/decay`; тесты (без сети): дедуп/reinforce-математика, decay-пороги, recall-скоринг, tombstone (6 тестов в `tests/test_lifecycle_lessons.rs`).
- **DoD:** 4 инструмента в контракте; повторный save того же контента возвращает `strengthened` и не создаёт дубль; decay не удаляет ничего; `cargo test`/`clippy` зелёные (52 набора тестов, 0 провалов).

### Фаза 65 — Слоты (slots): сервис, MCP, инъекция в контекст, синк (оценка 1–1.5 дня)

- [x] **65.1** `src/lifecycle/slots.rs`: валидация label/limit/scope; затенение project→global; append/replace c 413-семантикой (ошибка с размерами и подсказкой); доступ через ручной лок `conn_arc()` (паттерн SecretsService); seed-дефолты §5.3 лениво (`ensure_defaults`).
- [x] **65.2** MCP: `slot_list`, `slot_get`, `slot_create`, `slot_append`, `slot_replace`, `slot_delete` (контракт 45 → 51; project — явный аргумент или активный проект сессии). Операции в audit_log.
- [x] **65.3** `memory_context` + ресурс `memory://context`: блок `<agent_slots>` после памяти — pinned+непустые, алфавитный порядок, бюджет `OB2H_SLOTS_MAX_CHARS=1500` по границе целого слота (не влезающие пропускаются); при пустой памяти блок слотов отдаётся сам. Слоты не индексируются векторно (осознанно).
- [x] **65.4** Синк: `lessons`/`slots` в дельта-бандлы (LWW по updated_at/origin; reinforcements — монотонный max; no-op-гвард сравнивает значимые поля); counts/checksums в local-stats и verify; `audit_log` и `secrets` вне бандлов.
- [x] **65.5** Тесты: 8 в `tests/test_lifecycle_slots.rs` — затенение, 413+replace, read_only, seed+audit, бюджет/затенение инъекции, sync roundtrip PC↔VPS с LWW-конфликтом и монотонным reinforce.
- **DoD:** 6 инструментов; append при переполнении даёт читаемую ошибку с подсказкой; pinned-слоты видны в `<agent_slots>`; синк переносит слот PC↔VPS без потерь — **выполнено** (suite 53 набора зелёный, clippy чисто, python 20 OK, smoke MCP подтверждает seed/413/инъекцию).

### Фаза 66 — Хронология, рекап сессии, инкрементальные summaries (оценка 1–1.5 дня)

- [x] **66.1** `memory_timeline(anchor, before=5, after=5)`: якорь = ISO-дата либо подстрока диалога (первый хит по содержимому daily-логов); окно before+якорь+after; без якоря — хвост. Read-only поверх daily-логов (session_id/author из session_ingest-меты).
- [x] **66.2** `session_recap(session_id?, save=?)`: дайджест через llm_client (FakeLLM в тестах), extractive-фолбэк при недоступном LLM; `save=true` → факт в `memories` (category/source=recap, meta.source_ids = `daily/….jsonl#L…` — мост к провенансу Ф67); без сети в тестах.
- [x] **66.3** Инкрементальный курсор префикса рекапа в kv (`recap:<sid>` = {prefix, digest}): неизменённая сессия — кэш-хит без LLM-вызова (тест со счётчиком вызовов), растущая — только хвост. (Фазовый курсор дрима по history.jsonl уже давал prefix-skip для phase1 — задублирован не был.)
- [x] **66.4** Dream-слот-рефлекс за флагом `OB2H_DREAM_SLOTS` (дефолт off): TODO дня → global `pending_items` (append недостающих, cap 20, дедуп), счётчики → global `session_guidance`; вызывается в autodream после decay-sweep. Global-скоуп — у дрима нет проектного контекста (записано в PLAN.md §6).
- **DoD:** timeline возвращает корректное окно вокруг даты и вокруг текстового якоря; recap с FakeLLM детерминирован; повторный рекап на неизменённой сессии не делает LLM-вызов — **выполнено** (5 тестов; suite 54 набора зелёный, clippy чисто, python 20 OK, smoke memory_timeline — оба типа якорей).

### Фаза 67 — Провенанс фактов (оценка 0.5–1 день)

- [ ] **67.1** MCP `memory_verify(memory_id)`: по `memories.meta` (source/author/session_id) + source_ids (recap/lessons) собирает цитаты из daily-логов/сессий: id, timestamp, author, фрагмент (обрезка по budget), возвращает `citations`, `citation_count`; отсутствие метаданных — честный `provenance: none`, не ошибка.
- [ ] **67.2** Тесты: факт с сессионной метой → 1+ цитата; факт без меты → `provenance: none`; tombstone-факт → цитаты не теряются.
- **DoD:** 1 инструмент; verify отвечает за <50 мс на живой БД-копии (замер в отчёте фазы).

### Фаза 68 — Аудит-леджер (оценка 1 день)

- [ ] **68.1** `src/lifecycle/audit.rs`: единая точка `record_audit(actor, action, targets, details, tool)`; инструментирование существующих операций: `memory_forget` (+ новый опц. аргумент `reason` — governance-семантика, контракт аддитивен), `memory_feedback`, `memory_merge`, `lesson_*`, `slot_*`, `secrets_set/get/delete`, `workspace_write`.
- [ ] **68.2** MCP `memory_audit(operation?, limit=50)` — просмотр леджера (последние записи, фильтр по action); CLI `ob2h audit [--action …]`.
- [ ] **68.3** Ретеншн: записи старше 90 дней вычищаются в maintenance (id-шники, не контент); в бандлы синка не входит.
- **DoD:** 1 инструмент + reason у memory_forget; каждая операция из 68.1 оставляет запись; ротация не трогает свежие.

### Фаза 69 — Скрабинг секретов, bench-метрики recall-all/any-hit (оценка 1 день)

- [ ] **69.1** Редакция секретов в конвейерах: перед записью в daily/session_ingest и перед LLM-конвейерами (dream/consolidator/extractor) — маскирование по паттернам (`sk-…`, `ghp_…`, `Bearer …`, `://user:pass@`, `OB2H_[A-Z_]+=…`) и по именам записей Secrets Vault (имя известно без unlock); замена `[REDACTED:…]`; гейт `OB2H_REDACT=0`; тест-матрица паттернов.
- [ ] **69.2** `ob2h bench`: новые метрики **recall-all@k** (все релевантные источники кейса найдены в топ-k) и **any-hit@k** (хотя бы один) — golden-формат расширяется множеством релевантных id, старые кейсы (одно релевантное) читаются как раньше; вывод в отчёт и `bench/history.jsonl` (для гейта ADR-13 — пока наблюдение, порог не менять без данных).
- **DoD:** метрики считаются на старом и новом golden; скрабинг ловит тест-матрицу.

### Фаза 70 — Viewer: нативное окно egui (ядро, ручной запуск) (оценка 1.5–2 дня)

- [ ] **70.1** Каркас `src/viewer/` (`mod.rs` + `app.rs`): eframe-приложение под feature `gui`; тема RU/EN (`sys-locale`/`OB2H_UI_LANG`, тексты по образцу `pin_input::Texts`); авто-обновление 2 с + F5; read-only открытие БД (`SQLITE_OPEN_READ_ONLY`, WAL); single-instance через lockfile `data/viewer.lock` (pid + проверка живости); правило «ни одной записи» (тест: сессия просмотра не меняет БД).
- [ ] **70.2** Таб «Память»: FTS-поиск (memories_fts), фильтры проект/категория/источник/min trust, сортировка; панель деталей — content, meta (source/author/session/project), trust, связи из `memory_links`.
- [ ] **70.3** Таб «Граф»: узлы/рёбра выбранного проекта (`graph_nodes`/`graph_edges`), фоновый force-layout (spring embedding), cap `OB2H_VIEWER_GRAPH_MAX_NODES=300` (топ по степени), отрисовка `Painter` с pan/zoom, цвет ребра по типу; клик по узлу — детали.
- [ ] **70.4** Таб «Сессии»: список daily-логов workspace, ходы (user/assistant) выбранной сессии, FTS-поиск по логам — аналог «реплея» agentmemory (прокрутка событий без плеера).
- [ ] **70.5** Таб «Уроки/Слоты»: lessons (confidence/reinforcements/decay; tombstones скрыты) и slots (label/scope/pinned/заполненность) — данные появятся после Ф64/Ф65, до того заглушка «нет данных».
- [ ] **70.6** Таб «Статистика»: счётчики (memories/links/graph/lessons/slots/имена секретов — без значений), размер БД, `schema_version`, топ категорий и проектов.
- **DoD:** `cargo test --features gui` зелёные; окно открывается на живой копии БД; за сессию просмотра ни одной записи в БД; сборка без `gui` не меняется.

### Фаза 71 — Секреты во viewer: PIN-доступ, добавление, инициализация (оценка 1–1.5 дня)

- [ ] **71.1** Таб «Секреты» (`src/viewer/tabs/secrets.rs`): конечный автомат not_initialized / locked / unlocked поверх переиспользуемого `SecretsService` (владеет им процесс viewer); «Начать сохранять» → init (PIN дважды, окно PIN как в CLI); locked → поле PIN; unlocked → TTL-индикатор и кнопка «Заблокировать».
- [ ] **71.2** Список и операции: добавление записи (валидация имени как `secrets_set`), раскрытие значения с маской и авто-скрытием 15 с, копирование в буфер, удаление с подтверждением; каждая операция — через SecretsService + запись в `audit_log` (`actor=user`, `source_tool=viewer`).
- [ ] **71.3** Тесты безопасности: zeroize при lock/TTL/закрытии окна; значения не попадают в аудит/логи; неудачный PIN не пишет в БД; повторный init на существующем сейфе запрещён (подсказка «сейф уже создан»); unlock во viewer не влияет на lock-состояние `ob2h serve` (и наоборот).
- [ ] **71.4** headless-сборка: вкладка отвечает «недоступно в сборке без gui» (общее правило viewer, `[Error]`-стиль).
- **DoD:** полный цикл «Начать сохранять → добавить → unlock → показать → lock» во viewer; значения видны только после PIN и скрываются автоматически; MCP-контракт не растёт (56); аудит содержит операции viewer без значений.

### Фаза 72 — Viewer: активация из любого агента, доки, релиз 1.9.9 (оценка 1–1.5 дня)

- [ ] **72.1** MCP `viewer_open` (56-й инструмент): спавн detached-процесса `current_exe() viewer --data-dir …` (Windows: `CREATE_BREAKAWAY_FROM_JOB` + `DETACHED_PROCESS`); статусы `opened` / `already_running` / `gui_unavailable`; без feature `gui` — `[Error] … [Подсказка]` (честно, не паника).
- [ ] **72.2** CLI `ob2h viewer` — ручной запуск (главный путь пользователя); `ob2h doctor` — строка «Viewer: доступен / сборка без gui».
- [ ] **72.3** Живая проверка выживания окна: Hermes/ZCode/DSH завершают MCP-сессию — окно остаётся (детач); если harness убивает job-дерево и breakaway не проходит — зафиксировать поведение в доках, фолбэк — CLI-инструкция в скилле.
- [ ] **72.4** Скиллы (`skills/ob2h` + ZCode-вариант): питфоллы «показать память владельцу → `viewer_open`; headless/SSH — окно недоступно, сообщить честно» и «секреты — только через окно viewer/CLI по команде владельца; PIN в чате не запрашивать никогда».
- [ ] **72.5** Доки: README (новинки: lifecycle-пак, viewer, секреты во viewer + счётчик 56 инструментов), `CHANGELOG.md` `[1.9.9]` (контракт 41 → 56), `docs/HERMES_INTEGRATION.md` (§5.3 разделение ролей, `<agent_slots>`, viewer), `skills/ob2h/SKILL.md` (питфоллы: когда lesson_save vs memory_save, лимиты слотов), `docs/SYNC.md` (lessons/slots в бандлах, audit — нет).
- [ ] **72.6** Релиз 1.9.9: версия, тег, GitHub Release; **открытый вопрос CI**: релизная матрица по ADR-23 собирается без `gui` — добавить `--features gui` в Windows-артефакт (X11/Wayland на Windows не тянутся; для Linux при сложности — отдельный артефакт `ob2h-gui`). Решение — записать в `PLAN.md` §6 при реализации.
- **DoD:** `viewer_open` из Hermes/ZCode/DSH открывает окно (живая проверка); контракт-тест плагина — 56 инструментов; доки/CHANGELOG оформлены; релиз по §8.

---

## 7. Совместимость

| Контур | Влияние | Правило |
|---|---|---|
| MCP-контракт | 41 → 55, аддитивно | сигнатуры старых не менять; `memory_forget` получает **опциональный** `reason`; запись в CHANGELOG |
| Схема | M11, только CREATE TABLE/INDEX/TRIGGER | менять существующие таблицы запрещено; `schema_version` 10 → 11 |
| Синк PC↔VPS | lessons/slots добавляются в дельта-бандлы | поле-уровневый merge, LWW `updated_at`, conflicts.jsonl; audit/secrets вне бандлов (ADR-21 преемственность) |
| Дрим | decay-sweep уроков; опц. slot-reflect; инкрементальные summaries | всё за флагами/курсорами, возобновляемо (stdio-процесс может умереть) |
| Плагины Hermes/DSH | **код не меняется** | слоты доезжают через `memory_context`; уроки — через MCP-контракт |
| Viewer (Ф70–Ф72) | новый read-only процесс | feature `gui`; без портов/сети (ADR-7); вкладки памяти строго read-only; вкладка «Секреты» пишет только через SecretsService + audit_log (§5.5); single-instance lockfile; headless → честный `[Error]` |
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
| Ф69 Скрабинг/bench | 1 |
| Ф70 Viewer-окно (egui) | 1.5–2 |
| Ф71 Секреты во viewer | 1–1.5 |
| Ф72 Viewer-активация/доки/релиз | 1–1.5 |
| **Итого** | **9–12.5 фокусных дней** |

## 10. Общий DoD версии 1.9.9

1. `cargo test` и `cargo clippy --all-targets` зелёные (включая `--features gui`); Python-тесты плагина не сломаны (контракт-тест актуализирован до 56 инструментов).
2. Контракт MCP 41 → 56 задокументирован в `CHANGELOG.md`; tools/list отсортирован, ошибки — `[Error] … [Подсказка]`.
3. `ob2h bench` без деградации recall@5/MRR (гейт ADR-13); новые метрики recall-all@5/any-hit@5 выводятся.
4. Синк: `sync verify` зелёный с lessons/slots в бандле; audit_log и secrets вне бандлов.
5. Никаких автоудалений: decay даёт только кандидатов; tombstone — по явной команде.
6. Доки: README / HERMES_INTEGRATION / SYNC.md / скиллы обновлены; разделение ролей §5.3 зафиксировано.
7. Viewer: окно открывается из любого MCP-агента (`viewer_open`) и вручную (`ob2h viewer`); вкладки памяти/графа/сессий — ни одной записи в БД; без портов/сети; headless-сборка отвечает честным `[Error]`.
8. Секреты во viewer: значения видны только после локального ввода PIN в окне (ADR-18); кнопка «Начать сохранять» инициализирует сейф; значения не попадают в аудит/логи/daily/контекст LLM; MCP-контракт остаётся 56 инструментами.
