# Архитектура OmnesMemory

Целевая архитектура локального MCP-хранилища знаний для Hermes.
Решения зафиксированы в `PLAN.md` §1 (ADR-1…ADR-8), источники портирования —
в `REFERENCE_omnesbot.md`.

---

## 1. Компоненты

```
                       Hermes (Nous Research)
                         │  stdio, JSON-RPC (MCP)
                         ▼
┌────────────────────────────────────────────────────────────────┐
│ omnes_memory.server — FastMCP                                  │
│                                                                │
│  Инструменты (см. §4)                                          │
│  ├─ память:      memory_save/search/update/forget/context      │
│  ├─ workspace:   workspace_read/write, session_log             │
│  ├─ граф:        knowledge_extract, graph_search/reason/stats  │
│  ├─ дриминг:     dream_run/status/log/restore                  │
│  └─ сервис:      omnes_stats, omnes_backup                     │
│                                                                │
│  Сервисы                                                       │
│  ├─ memory_service ── гибридный поиск (FTS5+вектор, RRF k=60)  │
│  ├─ consolidator  ── суммаризация сессий по бюджету токенов    │
│  ├─ extractor     ── чанкинг → LLM-извлечение сущностей        │
│  ├─ graph_service ── граф знаний, дедуп, эмбеддинги узлов       │
│  ├─ dream         ── фаза 1 (анализ) + фаза 2 (агентные правки)│
│  ├─ autodream     ── фоновый поток (гейты 4ч/10 событий/lock)  │
│  ├─ llm_client    ── OpenAI-совместимый API (единственная сеть)│
│  └─ embedding     ── fastembed ONNX CPU | API /embeddings      │
│                                                                │
│  Хранилища                                                     │
│  ├─ SQLite data/omnes.db (WAL; FTS5-trigram; BLOB-вектора)     │
│  ├─ Файлы data/workspace/ (MD + history.jsonl + курсоры)       │
│  └─ git data/workspace/.git (авто-коммиты дрима, restore)      │
└────────────────────────────────────────────────────────────────┘
```

Жизненный цикл: Hermes спавнит процесс при старте (stdio), инструменты доступны
в чате; AutoDreamWorker живёт внутри процесса — работает, пока запущен Hermes.

## 2. Схема SQLite (`data/omnes.db`)

Все таблицы создаются версионными миграциями (`db.py`, версия в `kv.schema_version`).

```sql
-- Курсоры, версия схемы, размерность эмбеддингов, состояние autodream
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- Факты памяти (порт agent_memories из OmnesBOT)
CREATE TABLE memories (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  key TEXT UNIQUE NOT NULL,            -- стабильный ключ дедупа
  content TEXT NOT NULL,
  category TEXT DEFAULT 'general',
  importance REAL DEFAULT 0.5,         -- 0..1, decay + purge по порогам
  source TEXT,                         -- откуда: chat|dream|extract|manual
  meta TEXT,                           -- JSON
  embedding BLOB,                      -- float32[d], little-endian
  created_at TEXT, updated_at TEXT,
  access_count INTEGER DEFAULT 0,
  last_accessed TEXT
);

-- Связи между воспоминаниями (порт agent_memory_relations)
CREATE TABLE memory_relations (
  source_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
  target_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
  relation_type TEXT NOT NULL,
  weight REAL DEFAULT 1.0,
  UNIQUE (source_key, target_key, relation_type)
);

-- Инжест документов (фаза 4)
CREATE TABLE documents (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  title TEXT, path TEXT, meta TEXT,
  created_at TEXT
);
CREATE TABLE chunks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  text TEXT NOT NULL,
  embedding BLOB,
  created_at TEXT
);

-- Граф знаний (порт graph_nodes/graph_edges, PG-путь без Neo4j)
CREATE TABLE graph_nodes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  node_id TEXT UNIQUE NOT NULL,        -- sha256(label|type) — дедуп при экстракции
  label TEXT NOT NULL,
  node_type TEXT NOT NULL,             -- Person|Organization|Location|Event|Concept|Artifact|Other
  description TEXT,
  val INTEGER DEFAULT 1,               -- счётчик упоминаний (инкремент при дедупе)
  embedding BLOB,                      -- "{label}: {description}"
  created_at TEXT, updated_at TEXT
);
CREATE TABLE graph_edges (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
  target_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  weight REAL DEFAULT 1.0,
  contexts TEXT,                       -- JSON: строки-контексты упоминаний
  created_at TEXT,
  UNIQUE (source_id, target_id, label)
);

-- Журнал запусков дрима (фаза 5)
CREATE TABLE dream_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at TEXT, finished_at TEXT,
  status TEXT,                         -- running|ok|error
  trigger TEXT,                        -- manual|auto
  phase_log TEXT,                      -- JSON: [{phase, summary, files_changed}]
  stats TEXT                           -- JSON: записей обработано, правок, коммит
);

-- Полнотекстовый индекс: русский через trigram (проверено на целевом Python)
CREATE VIRTUAL TABLE memories_fts USING fts5(
  content, content='memories', content_rowid='id', tokenize='trigram'
);
CREATE VIRTUAL TABLE chunks_fts USING fts5(
  text, content='chunks', content_rowid='id', tokenize='trigram'
);
-- + триггеры INSERT/UPDATE/DELETE для синхронизации с основными таблицами
```

Индексы: `memories(category, importance DESC)`, `graph_nodes(label)`,
`graph_nodes(node_type)`, `graph_edges(source_id)`, `graph_edges(target_id)`.

## 3. Файловый workspace (`data/workspace/`)

Порт `MemoryStore` из OmnesBOT; отдельный git-репозиторий (не проекта):

```
workspace/
├── SOUL.md               # идентичность/характер (редко меняется, правится дримом)
├── USER.md               # факты о владельце
├── memory/
│   ├── MEMORY.md         # долгосрочные факты — инъектируются memory_context()
│   ├── history.jsonl     # консолидированные итоги сессий, {"cursor","timestamp","content"}
│   ├── .cursor           # последний обработанный консолидатором ход
│   └── .dream_cursor     # последний обработанный дримом ход
└── daily/                # MemoryV2-события по дням, food для автодрима
    └── YYYY-MM-DD.jsonl  # {"timestamp","query","answer_preview","source",...}
```

`memory_context()` собирает блок `<agent_memory>`: топ-факты по важности из
`memories` + текущий `MEMORY.md` — Hermes вставляет в промпт по своему усмотрению.

## 4. Контракт MCP-инструментов

Имена — snake_case с префиксом домена. Ответы компактные (Hermes режет вывод
на 50 000 байт; длинное — обрезается с маркером `…[truncated]`).

| Инструмент | Аргументы | Возвращает |
|---|---|---|
| `memory_save` | `content, key?, category?, importance?` | ключ, статус |
| `memory_search` | `query, limit?, mode?` (auto/fts/vector/hybrid) | топ-записи с score |
| `memory_update` | `key, content?, importance?` | статус |
| `memory_forget` | `key` | статус |
| `memory_context` | — | блок фактов для промпта |
| `workspace_read` | `file` (memory/soul/user/history) | содержимое |
| `workspace_write` | `file, content` | статус (+git-коммит) |
| `session_log` | `user_text, assistant_text, meta?` | статус + факт консолидации |
| `knowledge_extract` | `text \| file_path, max_chunks?` | статистика: сущности/отношения/чанки |
| `graph_search` | `query, limit?` | узлы+рёбра со скорингом |
| `graph_reason` | `query` | `{answer, confidence, used_entities, reasoning_steps}` |
| `graph_stats` | — | счётчики графа |
| `dream_run` | — | id запуска (фон) |
| `dream_status` | — | состояние, последний запуск |
| `dream_log` | `limit?` | история dream-коммитов |
| `dream_restore` | `commit` | статус отката |
| `omnes_stats` | — | счётчики всех хранилищ |
| `omnes_backup` | — | путь к бэкапу |

## 5. Потоки данных

**Память:** Hermes вызывает `memory_save`/`session_log` → запись + эмбеддинг →
`memory_search(query)` → FTS5 + cosine → RRF(k=60) → топ-K.

**Знания:** `knowledge_extract` → чанки (3000/300, границы предложений) → LLM-JSON
(сущности/отношения) → валидация + инференс отношений + фильтр мусора → upsert
в граф (дедуп по `node_id`, val++) → эмбеддинг узлов → `graph_reason` →
ретрив (граф 1-hop + вектор + FTS) → блок фактов → LLM-ответ.

**Дриминг:** daily-логи + history.jsonl накапливаются → `dream_run` (ручной или
автогейты ≥4ч/≥10 событий) → фаза 1: LLM-анализ новой истории на фоне текущих
MD-файлов → фаза 2: агентный цикл (≤10 итераций) с `_dream_read`/`_dream_edit` →
правки MEMORY/SOUL/USER → git auto-commit `dream: <дата>` → `dream_restore` откатывает.

## 6. Конфигурация (env, префикс `OMNES_`)

| Переменная | Дефолт | Назначение |
|---|---|---|
| `OMNES_DATA_DIR` | `<проект>/data` | БД + workspace + lock |
| `OMNES_LLM_BASE_URL` | `https://api.deepseek.com/v1` | OpenAI-совместимый API |
| `OMNES_LLM_API_KEY` | — | ключ (обязателен для фаз 3+) |
| `OMNES_LLM_MODEL` | `deepseek-v4-flash` | модель dream/extract/reason |
| `OMNES_EMBED_PROVIDER` | `local` | `local` (fastembed) \| `api` |
| `OMNES_EMBED_MODEL` | `intfloat/multilingual-e5-small` | для local; для api — имя модели |
| `OMNES_EMBED_BASE_URL` / `OMNES_EMBED_API_KEY` | — | для `api`-провайдера |
| `OMNES_CONTEXT_WINDOW` | `65536` | бюджет консолидатора |
| `OMNES_AUTODREAM_ENABLED` | `true` | фоновый дрим |
| `OMNES_RETENTION_DAYS` | `30` | ротация daily-логов |
| `OMNES_LOG_LEVEL` | `INFO` | логирование |

## 7. Надёжность

- SQLite WAL + `foreign_keys=ON`; все записи — короткие транзакции.
- Курсоры консолидации/дрима в kv-таблице и dot-файлах — любое прерывание
  возобновляется с места (идемпотентность по курсору).
- Lock-файл автодрима (stale 1ч) защищает от параллельных запусков.
- Бэкап: `VACUUM INTO` + копия workspace → `backups/`, ротация 14 копий.
- Логи: `logs/omnes-memory.log`, ротация по 5 МБ, 5 файлов.
