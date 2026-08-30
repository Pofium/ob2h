# 🗺️ План разработки OB2H v1.0: Память по проектам (AST-граф) и Мультиагентность

> Детальный архитектурный и инженерный план расширения OB2H до универсального хранилища знаний разработчика.
> Включает: детерминированный AST-анализ проектов (Tree-sitter без расхода токенов LLM), маркировку связей (EXTRACTED/INFERRED), выявление архитектурных хабов (God Nodes), пространства проектов (`project_id`) и поддержку 7+ ИИ-агентов (Hermes, Claude Code, Cursor, Windsurf, Gemini CLI, Qwen Code, OpenCode).

---

## 1. Архитектурный обзор v1.0

```
                   ┌─────────────────────────────────────────────────────────┐
                   │                     AI-АГЕНТЫ                           │
                   │  Hermes · Claude Code · Cursor · Windsurf · Gemini CLI  │
                   │           Qwen Code · MiMo · OpenCode · Antigravity     │
                   └───────────────────────────┬─────────────────────────────┘
                                               │ stdio MCP / JSON-RPC
                   ┌───────────────────────────▼─────────────────────────────┐
                   │                     OB2H Core (Rust)                    │
                   │             Single binary ~15MB · <15ms startup         │
                   ├─────────────────────────────┬───────────────────────────┤
                   │   Глобальная память (User)  │  Проектная память (Code)  │
                   │   - Fact store (memories)   │  - AST Extractor (No LLM) │
                   │   - Daily logs + Sessions   │  - Tree-sitter 35+ языков │
                   │   - MEMORY/SOUL/USER.md     │  - Code graph (EXTRACTED) │
                   │   - Personal entities       │  - Semantics (INFERRED)   │
                   │   - RRF k=60 (FTS5+Vector)  │  - God Nodes / Clusters   │
                   ├─────────────────────────────┴───────────────────────────┤
                   │                   SQLite (data/ob2h.db)                 │
                   │        WAL-mode · FTS5 trigram · BLOB float32 vectors   │
                   │        Миграция M3: project_id, provenance, confidence  │
                   ├─────────────────────────────────────────────────────────┤
                   │       AutoDream Worker · PC <-> VPS Bundle Sync         │
                   └─────────────────────────────────────────────────────────┘
```

---

## 2. Гарантии обновления и обратной совместимости (Zero Breaking Changes)

1. **Безопасность БД (Миграция M3):**
   - Автоматический бэкап `data/ob2h.db` в `backups/pre_m3_migration/` перед накатом миграции.
   - Аддитивные поля в таблицах: `project_id TEXT NULL`, `provenance TEXT DEFAULT 'manual'`, `confidence REAL DEFAULT 1.0`.
   - Существующие записи получают `project_id = NULL` (глобальная память) и остаются доступными для всех базовых запросов без изменений.
2. **Совместимость MCP-контракта:**
   - Все 19 существующих инструментов сохраняют 100% сигнатур и форматов ответов.
   - Параметр `project_id` во всех существующих инструментах является **опциональным**.
   - Новые инструменты добавляются аддитивно (общий пул вырастает до 24 инструментов).
3. **Обновление в 1 клик / 1 команду:**
   - `cargo build --release` → замена `ob2h.exe`.
   - Или скрипт `install.bat` / `ob2h install` / `ob2h agent install --all`.

---

## 3. Модель данных: Схема Миграции M3

### Таблица `projects` (новая)
```sql
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,               -- 'ob2h', 'omnes-aibot', 'client-app'
    name TEXT NOT NULL,
    root_path TEXT NOT NULL,           -- абсолютный путь к репозиторию
    description TEXT,
    tech_stack TEXT,                   -- JSON-массив: ["rust", "sqlite", "mcp"]
    active_branch TEXT,
    last_scanned_at DATETIME,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_projects_root_path ON projects(root_path);
```

### Расширение существующих таблиц (ALTER TABLE)
```sql
-- memories
ALTER TABLE memories ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);

-- documents & chunks
ALTER TABLE documents ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
ALTER TABLE chunks ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_documents_project ON documents(project_id);
CREATE INDEX IF NOT EXISTS idx_chunks_project ON chunks(project_id);

-- graph_nodes
ALTER TABLE graph_nodes ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
ALTER TABLE graph_nodes ADD COLUMN file_path TEXT;
ALTER TABLE graph_nodes ADD COLUMN line_start INTEGER;
ALTER TABLE graph_nodes ADD COLUMN line_end INTEGER;
ALTER TABLE graph_nodes ADD COLUMN provenance TEXT DEFAULT 'manual'; -- 'ast' | 'llm' | 'dialog' | 'manual'
ALTER TABLE graph_nodes ADD COLUMN is_god_node INTEGER DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_graph_nodes_project ON graph_nodes(project_id);
CREATE INDEX IF NOT EXISTS idx_graph_nodes_provenance ON graph_nodes(provenance);

-- graph_edges
ALTER TABLE graph_edges ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
ALTER TABLE graph_edges ADD COLUMN provenance TEXT DEFAULT 'manual'; -- 'ast' | 'llm' | 'dialog' | 'manual'
ALTER TABLE graph_edges ADD COLUMN confidence REAL DEFAULT 1.0;     -- 1.0 для AST, 0.7-0.9 для LLM
CREATE INDEX IF NOT EXISTS idx_graph_edges_project ON graph_edges(project_id);
```

---

## 4. Фазы реализации v1.0

### Фаза 10 — Пространства проектов и схема M3 (Оценка: 1 день)
- [ ] **10.1** Добавить миграцию `M3_projects_and_provenance` в `src/db/migrations.rs`.
- [ ] **10.2** Создать структуры и методы `ProjectService` в `src/project/`:
  - `register_project(id, name, path, description)`
  - `get_project(id_or_path)`
  - `list_projects()`
  - `detect_project_by_cwd(cwd) -> Option<Project>`
- [ ] **10.3** Обновить `MemoryService` и `GraphService` для поддержки фильтрации по `project_id` (при `None` — поиск по глобальной памяти либо по всем данным).
- [ ] **10.4** Тесты: накат M3 на заполненную M2 базу; проверка сохранности существующих воспоминаний; проверка создания проекта.

### Фаза 11 — Детерминированный AST-сканер кодовых баз (Оценка: 2-3 дня)
- [ ] **11.1** Подключить `tree-sitter` и грамматики ключевых языков (Rust, Python, TypeScript/JavaScript, Go, C/C++, SQL) через легковесные крейты.
- [ ] **11.2** Реализовать `AstCodeExtractor` в `src/project/ast/`:
  - Обход файлов с учетом `.gitignore` и лимитов размера.
  - Извлечение сущностей: `Module`, `Class`, `Struct`, `Trait`, `Interface`, `Function`, `Table`, `Endpoint`.
  - Извлечение связей: `IMPORTS`, `CALLS`, `IMPLEMENTS`, `DEFINES_FIELD`, `DEPENDS_ON`, `FOREIGN_KEY_TO`.
  - Запись связей в `graph_nodes`/`graph_edges` с `provenance = 'ast'` и `confidence = 1.0`.
- [ ] **11.3** Инкрементальное сканирование по SHA256 хэшам файлов: повторный скан обновляет только измененные файлы за миллисекунды.
- [ ] **11.4** Тесты: сканирование репозитория на Rust и Python; проверка детерминизма и отсутствия дублей связей.

### Фаза 12 — Анализ графа: God Nodes, Community Clusters и Architecture Reports (Оценка: 2 дня)
- [ ] **12.1** Реализовать в `src/graph/analytics.rs` алгоритмы графового анализа на чистом Rust:
  - Расчет степени связности (Degree & In-degree Centrality) для маркировки ключевых архитектурных хабов (`is_god_node = 1`).
  - Кластеризация подсистем (Label Propagation / Louvain).
- [ ] **12.2** Генератор архитектурного отчета `project_report`:
  - Сжатый markdown-дайджест структуры репозитория: ядровые модули, внешние зависимости, ключевые точки входа, схемы данных.
  - Экономия токенов контекста агента (~70x по сравнению с передачей сырых файлов).
- [ ] **12.3** Интерактивная визуализация графа (опционально):
  - Генерация автономного HTML-файла `graph.html` (vis.js/d3.js) с фильтрацией по проектам и типам связей (`EXTRACTED` vs `INFERRED`).

### Фаза 13 — Расширение MCP-интерфейса: 5 новых проектных инструментов (Оценка: 1 день)
- [ ] **13.1** `project_init(id, name, path, description)` — регистрация проекта и привязка каталога.
- [ ] **13.2** `project_scan(id, path?, incremental: true)` — запуск мгновенного AST-сканирования кодовой базы.
- [ ] **13.3** `project_context(id, task_description?)` — сборка компактного контекста проекта (`<project_context>`) для системного промпта агента.
- [ ] **13.4** `project_graph_search(id, query, provenance?, limit?)` — поиск по кодовому графу связей и зависимостей.
- [ ] **13.5** `project_report(id)` — получение готового архитектурного дайджеста и списка god-nodes.
- [ ] **13.6** Снапшот-тесты tools/list (24 инструмента) и интеграционные вызовы новых MCP-функций.

### Фаза 14 — Поддержка всех основных AI-агентов (Мультиагентность) (Оценка: 2 дня)

Реализовать единый менеджер конфигураций в `src/cli/agent.rs` (`ob2h agent ...`):

| Агент | Способ интеграции | Конфигурационный файл / директория |
|---|---|---|
| **Hermes** | MCP Server + Python Plugin + Skill | `~/.hermes/config.yaml`, `$HERMES_HOME/plugins/ob2h` |
| **Claude Code** | MCP Server + Skill (`/graphify`-style `/ob2h`) | `~/.claude/config.json`, `~/.claude/skills/ob2h/SKILL.md` |
| **Cursor** | MCP Stdio Server | `.cursor/mcp.json` или `~/.cursor/mcp.json` |
| **Windsurf / Cascade** | MCP Stdio Server | `~/.codeium/windsurf/mcp_config.json` |
| **Gemini CLI / Antigravity** | MCP Stdio Server + Skill | `mcp_config.json`, `.gemini/config/` |
| **Qwen Code / OpenCode / MiMo** | MCP Stdio Server | `.mcp.json` / `opencode.json` |

- [ ] **14.1** CLI подкоманда `ob2h agent install`:
  - `ob2h agent install --agent claude` (настройка `~/.claude/config.json` + установка скилла `ob2h`).
  - `ob2h agent install --agent cursor` (генерация `.cursor/mcp.json` в текущем проекте или глобально).
  - `ob2h agent install --agent windsurf`
  - `ob2h agent install --agent gemini`
  - `ob2h agent install --agent hermes` (вызывает существующий пайплайн).
  - `ob2h agent install --all` (автоматический детект установленных агентов и регистрация везде).
- [ ] **14.2** CLI подкоманда `ob2h agent status` — сводная таблица обнаруженных и настроенных агентов.
- [ ] **14.3** Скиллы для агентов:
  - `skills/claude/SKILL.md` (поддержка команд `/ob2h`, `/project-scan`, `/memory`).
  - `skills/gemini/SKILL.md`
  - `skills/hermes/SKILL.md`

### Фаза 15 — Интеграция с Дримингом и Синхронизацией (Оценка: 1 день)
- [ ] **15.1** `AutoDreamWorker`: во время фонового дриминга сопоставлять коммиты проектов и задачи из `daily.jsonl` с узлами графа, создавая связи `RESOLVED_BY`, `MODIFIED_IN_COMMIT`.
- [ ] **15.2** `SyncService`: включение полей `project_id`, `provenance`, `confidence` в JSONL-бандлы gzip (`ob2h sync push/pull`) с сохранением LWW-конфликтологии.
- [ ] **15.3** E2E-тест: синхронизация проектного графа между PC и VPS.

---

## 5. Итоговый контракт инструментов MCP (24 инструмента)

### Базовая память и воркспейс (1-8)
1. `memory_save(content, key?, category?, importance?, project_id?, source?)`
2. `memory_search(query, limit?, mode?, project_id?)`
3. `memory_update(key, content?, importance?, category?, project_id?)`
4. `memory_forget(key)`
5. `memory_context(query?, max_tokens?, project_id?)`
6. `workspace_read(file)`
7. `workspace_write(file, content, commit_message)`
8. `session_log(user_text, assistant_text, project_id?, source?)`

### Семантический граф и KAG (9-12)
9. `knowledge_extract(text?, file_path?, max_chunks?, project_id?)`
10. `graph_search(query, limit?, project_id?, provenance?)`
11. `graph_reason(query, project_id?)`
12. `graph_stats(project_id?)`

### Дриминг и обслуживание (13-18)
13. `dream_run(background?)`
14. `dream_status()`
15. `dream_log(limit?)`
16. `dream_restore(commit)`
17. `omnes_stats()`
18. `omnes_backup()`

### Массовый инжест (19)
19. `session_ingest(messages, session_id?, project_id?, source?)`

### Проектная память и AST-анализ (20-24, НОВЫЕ)
20. `project_init(id, name, path, description?)`
21. `project_scan(id, path?, incremental?)`
22. `project_context(id, task_description?)`
23. `project_graph_search(id, query, provenance?, limit?)`
24. `project_report(id)`

---

## 6. Чеклист готовности (Definition of Done для v1.0)

- [ ] Все юнит- и интеграционные тесты Rust (`cargo test`) зелёные (≥120 тестов).
- [ ] Линтер `cargo clippy --all-targets` проходит без предупреждений.
- [ ] Миграция M3 протестирована на совместимость со старыми БД v0.8/v0.9.
- [ ] AST-парсер мгновенно сканирует кодовые базы на Rust, Python, TS/JS без сетевых обращений к LLM.
- [ ] `ob2h agent install --all` успешно конфигурирует все доступные агенты.
- [ ] Обновлены `README.md`, `CHANGELOG.md`, `docs/ARCHITECTURE.md`.
