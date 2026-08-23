# Архитектура OB2H

Архитектура локального MCP-хранилища знаний для Hermes (Rust, v0.9).
Решения зафиксированы в `PLAN.md` §1 (ADR-1…ADR-8) и §6 (ADR-9…ADR-11:
синхронизация, MemoryProvider-плагин, tombstones). Источники портирования —
`REFERENCE_omnesbot.md`.

---

## 1. Компоненты

Два способа подключения к Hermes (подробно — `HERMES_INTEGRATION.md`):

```
            Режим A (целевой): MemoryProvider-плагин          Режим 0/фолбэк: MCP stdio
            ┌──────────────────────────────┐                 ┌──────────────────────┐
            │ Hermes ($HERMES_HOME/        │                 │ Hermes config.yaml   │
            │   plugins/ob2h/, Python)     │                 │   mcp_servers.ob2h   │
            │  prefetch → <agent_memory>   │                 │  mcp__ob2h__* tools  │
            │  sync_turn → session_ingest  │                 └──────────┬───────────┘
            │  get_tool_schemas → ob2h-*   │                            │ JSON-RPC/stdio
            └───────────────┬──────────────┘                            │
                            │ JSON-RPC/stdio (долгоживущий subprocess)  │
                            ▼                                           ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│ ob2h serve (один Rust-бинарник; конкурентная обработка JSON-RPC)                │
│                                                                                  │
│  Инструменты (см. §4):                                                          │
│  ├─ память:      memory_save/search/update/forget/context                       │
│  ├─ workspace:   workspace_read/write, session_log, session_ingest              │
│  ├─ граф:        knowledge_extract, graph_search/reason/stats                   │
│  ├─ дриминг:     dream_run/status/log/restore                                   │
│  └─ сервис:      omnes_stats, omnes_backup                                      │
│                                                                                  │
│  Сервисы (src/)                                                                 │
│  ├─ memory/     ── гибридный поиск (FTS5+вектор, RRF k=60), tombstones          │
│  ├─ consolidator── суммаризация сессий по бюджету токенов                       │
│  ├─ extractor   ── чанкинг → LLM-извлечение сущностей (OneKE-lite)              │
│  ├─ graph/      ── граф знаний KAG-lite, дедуп sha256(label|type), эмбеддинги   │
│  ├─ dream/      ── фаза 1 (анализ) + фаза 2 (агентные правки) + autodream       │
│  │                 (гейты 4ч/10 событий/lock; maintenance: decay/purge/tombstones)│
│  ├─ sync/       ── бандлы PC↔VPS: export/import/push/pull, LWW (ADR-9)          │
│  ├─ llm/        ── OpenAI-совместимый API (единственная сеть)                   │
│  ├─ embedding/  ── Candle MiniLM (in-process, CPU) | API /embeddings            │
│  └─ backup/     ── VACUUM INTO + workspace, ротация 14                          │
│                                                                                  │
│  Хранилища                                                                       │
│  ├─ SQLite data/ob2h.db (WAL, busy_timeout; FTS5-trigram; BLOB-вектора)         │
│  ├─ Файлы data/workspace/ (MD + history.jsonl + daily/*.jsonl + курсоры)        │
│  ├─ git data/workspace/.git (авто-коммиты дрима, restore)                       │
│  └─ data/sync/ (peers.json, бандлы outbox/inbox)                                │
└─────────────────────────────────────────────────────────────────────────────────┘
```

Жизненный цикл: процесс поднимается Hermes'ом (MCP-спавн или плагин), инструменты
доступны в чате; AutoDreamWorker и after_dream-синк живут внутри процесса.
CLI (`ob2h …`) открывает ту же БД параллельно — WAL + busy_timeout это позволяют.

## 2. Схема SQLite (`data/ob2h.db`)

Версионные миграции в `src/db/schema.rs` (версия в `kv.schema_version`).
M1 — базовые таблицы; M2 (v0.9) — аддитивные колонки синхронизации + `sync_state`,
перед применением на живой БД создаётся бэкап `backups/pre-v08-*.db`.

```sql
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- версия схемы, размерность эмбеддингов, дедуп session_ingest, watermark'ы…

-- Факты памяти (порт agent_memories из OmnesBOT)
CREATE TABLE memories (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  key TEXT UNIQUE NOT NULL,            -- стабильный ключ дедупа
  content TEXT NOT NULL,
  category TEXT DEFAULT 'general',
  importance REAL DEFAULT 0.5,         -- 0..1, decay + purge по порогам
  source TEXT,                         -- chat|dream|extract|manual|sync|hermes-builtin
  meta TEXT,
  embedding BLOB,                      -- float32[d], little-endian
  created_at TEXT, updated_at TEXT,
  access_count INTEGER DEFAULT 0,
  last_accessed TEXT,
  -- M2 (синк):
  origin TEXT NOT NULL DEFAULT '',     -- '' = «этот узел»; иначе pc/vps/…
  deleted_at TEXT                      -- tombstone: скрыт из поиска, реплицируется
);

CREATE TABLE memory_relations (
  source_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
  target_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
  relation_type TEXT NOT NULL,
  weight REAL DEFAULT 1.0,
  UNIQUE (source_key, target_key, relation_type)
);  -- локальная, не синхронизируется (выводится дримом заново)

CREATE TABLE documents (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  title TEXT, path TEXT, meta TEXT, created_at TEXT
);  -- не синхронизируются: ре-ингестируемы knowledge_extract
CREATE TABLE chunks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL, text TEXT NOT NULL,
  embedding BLOB, created_at TEXT
);

-- Граф знаний (PG-путь KAG без Neo4j)
CREATE TABLE graph_nodes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  node_id TEXT UNIQUE NOT NULL,        -- sha256(label|type) — дедуп при экстракции
  label TEXT NOT NULL,
  node_type TEXT NOT NULL,             -- Person|Organization|Location|Event|Concept|Artifact|Other
  description TEXT,
  val INTEGER DEFAULT 1,               -- счётчик упоминаний
  embedding BLOB,                      -- "{label}: {description}"
  created_at TEXT, updated_at TEXT,
  origin TEXT NOT NULL DEFAULT '',     -- M2
  deleted_at TEXT                      -- M2
);
CREATE TABLE graph_edges (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
  target_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  weight REAL DEFAULT 1.0,
  contexts TEXT,                       -- JSON: строки-контексты упоминаний
  created_at TEXT,
  updated_at TEXT NOT NULL DEFAULT '', -- M2 (backfill из created_at)
  origin TEXT NOT NULL DEFAULT '',     -- M2
  deleted_at TEXT,                     -- M2
  UNIQUE (source_id, target_id, label)
);

CREATE TABLE dream_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at TEXT, finished_at TEXT,
  status TEXT, trigger TEXT, phase_log TEXT, stats TEXT
);

-- M2: состояние синхронизации (watermark на пира + идемпотентность импортов)
CREATE TABLE sync_state (
  peer TEXT PRIMARY KEY,               -- имя пира или '__imports' (журнал применённых)
  last_export_at TEXT,
  last_import_at TEXT,
  applied_bundles TEXT NOT NULL DEFAULT '[]'
);

-- Полнотекстовый индекс: русский через trigram
CREATE VIRTUAL TABLE memories_fts USING fts5(
  content, content='memories', content_rowid='id', tokenize='trigram'
);
CREATE VIRTUAL TABLE chunks_fts USING fts5(
  text, content='chunks', content_rowid='id', tokenize='trigram'
);
-- + триггеры INSERT/UPDATE/DELETE
```

Индексы: `memories(category, importance DESC)`, `graph_nodes(label)`,
`graph_nodes(node_type)`, `graph_edges(source_id)`, `graph_edges(target_id)`.

## 3. Файловый workspace (`data/workspace/`)

Порт `MemoryStore` из OmnesBOT; отдельный git-репозиторий (не проекта):

```
workspace/
├── SOUL.md               # идентичность/характер (правится дримом)
├── USER.md               # факты о владельце
├── memory/
│   ├── MEMORY.md         # долгосрочная выжимка — входит в memory_context()
│   ├── history.jsonl     # консолидированные итоги сессий
│   ├── .cursor           # последний обработанный консолидатором ход
│   └── .dream_cursor     # последний обработанный дримом ход
└── daily/                # события по дням — пища автодрима/dream-extract
    └── YYYY-MM-DD.jsonl  # {"timestamp","user_text","assistant_text","meta"}
```

Workspace и daily-логи **не синхронизируются** — это локальные представления;
после мержа БД дрим каждой машины перегенерирует свои MD из общей памяти.

## 4. Контракт MCP-инструментов (19)

Имена — snake_case с префиксом домена. Ответы компактные (лимит Hermes 50 000 байт;
длинное обрезается с `…[truncated]`). Ошибки — строка `[Error] …`, не исключение.
Плагин (Mode A) не экспонирует модели: `session_log`, `session_ingest` (автоматика)
и `memory_search`/`memory_context` (замещены prefetch-инъекцией).

| Инструмент | Аргументы | Возвращает |
|---|---|---|
| `memory_save` | `content, key?, category?, importance?, source?` | ключ, статус |
| `memory_search` | `query, limit?, mode?` (hybrid/fts/vector) | топ-записи с score |
| `memory_update` | `key, content?, importance?, category?` | статус |
| `memory_forget` | `key` | статус (tombstone) |
| `memory_context` | `query?, max_tokens?` | блок `<agent_memory>` |
| `workspace_read` | `file` (memory/soul/user/history) | содержимое |
| `workspace_write` | `file, content, commit_message?` | статус (+git-коммит) |
| `session_log` | `user_text, assistant_text, source?` | статус + консолидация |
| `session_ingest` | `messages[{role,content}], source?, session_id?` | пары + дедуп по позиции |
| `knowledge_extract` | `text \| file_path, max_chunks?` | сущности/отношения/чанки |
| `graph_search` | `query, limit?` | узлы+рёбра со скорингом |
| `graph_reason` | `query` | `{answer, confidence, used_entities, steps}` |
| `graph_stats` | — | счётчики графа |
| `dream_run` | `background?` | id запуска |
| `dream_status` | — | состояние, гейты |
| `dream_log` | `limit?` | история dream-коммитов |
| `dream_restore` | `commit` | статус отката |
| `omnes_stats` | — | счётчики хранилищ |
| `omnes_backup` | — | путь к бэкапу |

## 5. Потоки данных

**Память (автоматическая, Mode A):** каждый ход — плагин `sync_turn` →
`session_ingest` (полный префикс сессии; сервер пишет только хвост по дедупу) →
daily-лог; перед ходом плагин `prefetch` → `memory_context`/`memory_search` →
инъекция `<agent_memory>` (+ `recall_status` 🧠). Явное сохранение — `memory_save`.

**Поиск:** `memory_search(query)` → FTS5 (trigram) + cosine по BLOB → RRF(k=60) →
топ-K. Tombstones отфильтрованы на всех ветках.

**Знания:** `knowledge_extract` → чанки (3000/300, границы предложений) → LLM-JSON
→ валидация + инференс отношений (~44 шаблона) + фильтр мусора → upsert в граф
(дедуп `node_id`, val++) → эмбеддинг узлов → `graph_reason` → ретрив
(граф 1-hop + вектор + FTS) → LLM-ответ с confidence.

**Дриминг:** daily-логи накапливаются → автогейты ≥4ч/≥10 событий → фаза 1: LLM-анализ;
фаза 2: агентный цикл правки MD (≤10 итераций) → dream-extract: сущности из сессий
в общий граф → git auto-commit → `dream_restore` откатывает. Maintenance:
decay важности, purge слабых (tombstone), физическая чистка tombstones
(retention×2 — чтобы удаления успели уйти пирам).

**Синхронизация (ADR-9):** `sync export` — строки четырёх синхронизируемых таблиц
с `MAX(updated_at, deleted_at) >= watermark(пира)` → gzip-бандл (hex-эмбеддинги
внутри; origin нормализуется из ''). `sync import` — транзакция целиком: LWW
(`updated_at`, tie-break `priority` по origin), no-op для идентичных строк,
tombstones, идемпотентность `applied_bundles`, авто-бэкап перед новым бандлом.
Транспорт: scp (peer method=ssh) или manual. Полный гайд — `SYNC.md`.

## 6. Конфигурация (env, префикс `OB2H_`)

| Переменная | Дефолт | Назначение |
|---|---|---|
| `OB2H_DATA_DIR` | `<проект>/data` | БД + workspace + sync + lock |
| `OB2H_LLM_BASE_URL` | `https://api.deepseek.com/v1` | OpenAI-совместимый API |
| `OB2H_LLM_API_KEY` | — | **имя env-переменной** с ключом (индрекция; фолбэк — литерал) |
| `OB2H_LLM_MODEL` | `deepseek-v4-flash` | модель dream/extract/reason |
| `OB2H_EMBED_PROVIDER` | `local` | `local` (Candle, in-process) \| `api` \| `fake` (тесты) |
| `OB2H_EMBED_MODEL` | `paraphrase-multilingual-MiniLM-L12-v2` | для local; для api — имя модели |
| `OB2H_EMBED_BASE_URL` / `OB2H_EMBED_API_KEY` | — | для `api`-провайдера |
| `OB2H_CONTEXT_WINDOW` | `65536` | бюджет консолидатора |
| `OB2H_AUTODREAM_ENABLED` | `true` | фоновый дрим (+ after_dream синк) |
| `OB2H_DREAM_EXTRACT_ENABLED` | `true` | извлечение сущностей из сессий в граф |
| `OB2H_RETENTION_DAYS` | `30` | ротация daily-логов; tombstones ×2 |
| `OB2H_LOG_LEVEL` | `INFO` | логирование |

**Эмбеддинги — варианты:**

| Вариант | Качество (рус) | Зависимости | Когда использовать |
|---|---|---|---|
| `local` + MiniLM-L12-v2 (дефолт, 384d) | хорошее | ничего — чистый Rust/Candle, кэш `~/.cache/huggingface` | всегда; одинаковые векторы на всех машинах (важно для синка) |
| `api` + LM Studio / Ollama / любой OpenAI-совместимый | зависит от модели | внешний сервер | если локальная модель уже крутится; при смешивании провайдеров недостающие векторы пересчитываются на приёмнике |

**Синхронизация** настраивается не env, а `data/sync/peers.json`
(origin, priority, peers, after_dream) — см. `SYNC.md`.

## 7. Надёжность

- SQLite WAL + `foreign_keys=ON` + `busy_timeout=5000` (несколько процессов:
  плагин + mcp_servers + CLI); все записи — короткие транзакции.
- Курсоры консолидации/дрима — идемпотентность после любого сбоя; lock автодрима
  (stale 1ч) защищает от параллельных запусков.
- Бэкап: `VACUUM INTO` + workspace → `backups/`, ротация 14; дополнительно перед
  миграцией M2 и перед каждым новым синк-бандлом.
- Миграции аддитивные и даунгрейт-безопасные (новые колонки с DEFAULT; старый
  бинарник продолжает работать).
- Логи: `data/logs/ob2h.log`.
