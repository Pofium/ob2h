# 🚀 План разработки OB2M / OB2H v1.2: Автоматизация, Автодетект проектов и Интеллектуальный Граф

> **Версия плана:** 1.2.0  
> **Статус:** Утверждён к реализации  
> **Предыдущие этапы:** v0.8/0.9 (Ядро, Память, Дриминг, Синк), v1.0/1.1 (AST-граф 8 языков, God Nodes, мультиагентность)  
> **Принцип совместимости:** 100% обратная совместимость (Zero Breaking Changes) для всех 24 существующих инструментов MCP.

---

## 1. Архитектурный обзор v1.2

```
                       ┌────────────────────────────────────────────────────────┐
                       │                   AI-АГЕНТЫ В РАБОТЕ                   │
                       │ Claude Code · Cursor · Windsurf · Hermes · Antigravity │
                       │           Qwen Code · MiMo · OpenCode · Gemini         │
                       └───────────────────────────┬────────────────────────────┘
                                                   │ stdio MCP (initialize: rootUri, workspaceFolders)
                       ┌───────────────────────────▼────────────────────────────┐
                       │                СЕССИОННЫЙ СЛОЙ MCP                     │
                       │ - Session Auto-Binding (привязка к текущему workspace)  │
                       │ - Zero-Config Auto-Init (автодетект манифестов корня)  │
                       │ - Неявная подстановка active_project_id во все тулы    │
                       ├────────────────────────────────────────────────────────┤
                       │             ФОНОВЫЙ СЛОЙ АВТОМАТИЗАЦИИ                 │
                       │ - File Watcher (notify): авто-перепарсинг при сейве    │
                       │ - Git Lifecycle Hooks: post-commit / post-checkout      │
                       │ - AutoSync Worker: фоновый PC <-> VPS обмен бандлами   │
                       ├────────────────────────────────────────────────────────┤
                       │                  ИНКРЕМЕНТАЛЬНЫЙ AST                   │
                       │ - ignore crate: уважение всех уровней .gitignore       │
                       │ - project_files: sha256 хэши, дельта-скан за 5-15 мс   │
                       │ - Очистка удалённых файлов и узлов                     │
                       ├────────────────────────────────────────────────────────┤
                       │            СЕМАНТИКА & АНАЛИТИКА ГРАФА                 │
                       │ - Гибридный поиск по коду: AST + Candle MiniLM + FTS5  │
                       │ - Blast Radius (project_impact): радиус поломки кода   │
                       │ - Детектор циклических зависимостей                    │
                       │ - MCP Resources & Prompts (project://, memory://)      │
                       ├────────────────────────────────────────────────────────┤
                       │                 SQLite (data/ob2h.db)                  │
                       │      Миграция M4: project_files + embedding для AST    │
                       └────────────────────────────────────────────────────────┘
```

---

## 2. Модель данных: Схема Миграции M4

### Таблица `project_files` (новая)
Хранит хэши и метаданные просканированных файлов для честного дельта-сканирования:
```sql
CREATE TABLE IF NOT EXISTS project_files (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    lines_count INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (project_id, rel_path)
);
CREATE INDEX IF NOT EXISTS idx_project_files_project ON project_files(project_id);
```

### Расширение `graph_nodes` для семантического поиска по коду
```sql
-- Добавление эмбеддинга для узлов кода (функций, структур, интерфейсов)
-- BLOB float32 вектор (384d Candle MiniLM)
ALTER TABLE graph_nodes ADD COLUMN embedding BLOB;
```

---

## 3. Детальные фазы реализации

### Фаза 16 — Zero-Config Автодетект проектов и Сессионный Контекст (Оценка: 1.5 дня)

Цель: Агенту больше не нужно передавать `project_id` вручную, а разработчику — вызывать `project_init`.

- [x] **16.1** Захват контекста рабочей директории в MCP `initialize`:
  - В `src/mcp/server.rs` парсить `params.rootUri`, `params.rootPath` и `params.workspaceFolders`.
  - Преобразовывать URI (`file:///...`) в канонический путь файловой системы с нормализацией слэшей.
  - Fallback: если клиент не передал workspace (напр. raw stdio), использовать `std::env::current_dir()`.
  - Хранить в состоянии `McpServer` потокобезопасный `current_workspace: Arc<RwLock<Option<PathBuf>>>`.
- [x] **16.2** Умный поиск корня проекта (`find_project_root`):
  - Реализовать функцию обхода вверх по дереву каталогов от рабочей папки до ближайшего маркера проекта:
    - Git: наличие `.git` (директории или файла gitdir для worktree).
    - Манифесты: `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `composer.json`, `pubspec.yaml`, `pom.xml`, `build.gradle`, `mix.exs`.
- [x] **16.3** Детектор метаданных и Zero-Config Auto-Init (`auto_register_or_detect`):
  - При первом обращении агента из каталога, если проект не зарегистрирован в таблице `projects`:
    - Извлекать имя проекта:
      - `Cargo.toml` $\rightarrow$ `package.name`
      - `package.json` $\rightarrow$ `name`
      - `pyproject.toml` $\rightarrow$ `project.name` / `tool.poetry.name`
      - `go.mod` $\rightarrow$ последняя часть пути модуля
      - `composer.json` $\rightarrow$ `name`
      - fallback $\rightarrow$ имя папки корня репозитория.
    - Автоматически определять стек технологий (массив `tech_stack`): `["rust"]`, `["node", "typescript"]`, `["python"]`, `["php"]` и т.д.
    - Считывать активную ветку из `.git/HEAD`.
    - Извлекать краткое описание из манифеста или первой строки `README.md`.
    - Регистрировать проект в SQLite с автосгенерированным `id` (slug имени).
- [x] **16.4** Неявная подстановка `active_project_id` в вызовы инструментов:
  - В `server.rs::call_tool` для инструментов:
    - `memory_save`, `memory_search`, `memory_update`, `memory_context`
    - `session_log`, `session_ingest`
    - `knowledge_extract`, `graph_search`, `graph_reason`, `graph_stats`
    - `project_context`, `project_graph_search`, `project_report`
  - Если аргумент `project_id` опущен (`None` или пустая строка), автоматически подставлять ID детектированного проекта текущей сессии.
- [x] **16.5** Тесты:
  - Эмуляция `initialize` с разными `rootUri` (Windows-пути, URL-encoded пути).
  - Тест автодетекта для Rust, Node, Python и вложенных монорепозиториев.
  - Тест вызова `memory_save` без `project_id` с проверкой привязки к проекту в БД.

---

### Фаза 17 — Честный инкрементальный AST-скан и учёт `.gitignore` (Оценка: 2 дня)

Цель: Сканирование 10 000 файлов за миллисекунды, строгое следование правилам игнорирования.

- [x] **17.1** Подключение крейта `ignore` в `Cargo.toml`:
  - Использовать `ignore::WalkBuilder` (проверенный движок `ripgrep`).
  - Поддержка `.gitignore`, глобальных `core.excludesFile`, правил `.ignore`, скрытых файлов и бинарных файлов.
  - Сохранение фильтрации поддерживаемых расширений (`rs`, `py`, `ts`, `tsx`, `js`, `jsx`, `go`, `sql`, `php`, `dart`, `java`, `c`, `cpp`, `h`, `hpp`).
- [x] **17.2** Таблица `project_files` и миграция M4:
  - Добавить миграцию M4 в `src/db/schema.rs`.
  - Методы в `ProjectService`:
    - `get_known_files(project_id) -> HashMap<String, (String, i64)>` (путь $\rightarrow$ (sha256, mtime)).
    - `save_file_hashes(project_id, Vec<FileMetadata>)`.
    - `delete_file_nodes(project_id, rel_path)`.
- [x] **17.3** Инкрементальный алгоритм дельта-сканирования:
  - В `AstCodeExtractor::scan_directory`:
    1. Обход файлов через `ignore::WalkBuilder`.
    2. Сравнение быстрого mtime/размера и при несовпадении — вычисление SHA256.
    3. Разделение списка файлов на 3 корзины:
       - **Unchanged**: пропускаются без чтения содержимого.
       - **Modified / New**: парсинг в AST, удаление предыдущих узлов файла, вставка новых узлов и рёбер.
       - **Deleted**: удаление узлов и рёбер (`WHERE file_path = ? AND project_id = ?`).
  - Учёт удаления связей, ведущих к удалённым узлам.
- [x] **17.4** Повторный пересчёт God Nodes:
  - Вызывать `GraphAnalytics::update_god_nodes` только если были изменены или удалены файлы.
- [x] **17.5** Тесты:
  - Первичный скан $\rightarrow$ фиксация времени и числа узлов.
  - Повторный скан без изменений $\rightarrow$ 0 перепарсенных файлов, время < 20 мс.
  - Изменение 1 файла $\rightarrow$ обновляются только связанные узлы, хэш обновлён.
  - Удаление файла $\rightarrow$ узлы файла исчезают из графа.

---

### Фаза 18 — Реактивная автоматизация: File Watcher, Git Hooks, AutoSync, Doctor (Оценка: 2-3 дня)

Цель: Граф всегда актуален в реальном времени, фоновая репликация между машинами.

- [ ] **18.1** Фоновый File Watcher (`notify` crate):
  - Подключить `notify` и `notify-debouncer-mini` в `Cargo.toml`.
  - Создать модуль `src/project/watcher.rs`.
  - При запуске `ob2h serve` поднимать фоновую задачу tokio для активного проекта:
    - Фильтрация событий (только расширения кода, игнор `.git`, `target`, `node_modules`).
    - Debounce 2.5 секунды (чтобы дождаться окончания серии правок от IDE или агента).
    - Автоматический инкрементальный до-скан изменённых файлов без блокировки основного потока MCP.
    - Включение/отключение через конфиг `OB2H_WATCHER_ENABLED=true|false` (по умолчанию `true`).
- [ ] **18.2** Git Lifecycle Hooks (`ob2h project hook install`):
  - CLI подкоманда `ob2h project hook install [--path <repo>]`:
    - Создаёт `.git/hooks/post-commit` и `.git/hooks/post-merge`.
    - Скрипт хука: быстрый вызов `ob2h project scan --id <id> --incremental` (тихий запуск).
  - При смене веток (`post-checkout`) обновлять поле `active_branch` в таблице `projects`.
- [ ] **18.3** Фоновый AutoSync Worker (PC $\leftrightarrow$ VPS):
  - Модуль `src/sync/worker.rs` (`AutoSyncWorker`):
    - Фоновый поток в `ob2h serve`, проверяющий статус раз в 30 минут.
    - Гейты запуска:
      1. Наличие валидного `data/sync/peers.json`.
      2. Прошло $\ge$ N минут с прошлого синка (конфиг `OB2H_SYNC_INTERVAL_MINUTES=120`).
      3. Были локальные изменения (новые воспоминания / сессии / правки графа) ИЛИ событие `after_dream`.
    - Вызов `SyncManager::push` и `SyncManager::pull` через системный SSH без всплывающих окон.
    - Защита от падений: сетевые ошибки логируются как warning, процесс сервера не прерывается.
- [ ] **18.4** CLI-команда диагностики `ob2h doctor`:
  - Проверка и цветной вывод статуса:
    - Доступность SQLite, статус WAL, размер базы, свободное место на диске.
    - FTS5 trigram и статус векторных расширений/моделей Candle.
    - Список подключенных AI-агентов:
      - Проверка Claude (`~/.claude/config.json`)
      - Проверка Cursor (`.cursor/mcp.json` / global)
      - Проверка Windsurf (`~/.codeium/windsurf/mcp_config.json`)
      - Проверка Gemini / Antigravity (поддержка обоих путей: `.gemini/antigravity/` и `.gemini/antigravity-ide/`)
      - Проверка Qwen, OpenCode, Hermes.
    - Статус Git-хуков и пирингов синхронизации.
    - Интерактивное предложение починить расхождения (автоматический фикс путей).
- [ ] **18.5** Тесты:
  - Эмуляция файловых изменений и проверка реакции watcher'а через каналы `tokio::sync::mpsc`.
  - Тест генерации файлов Git-хуков.
  - Тест вывода `ob2h doctor`.

---

### Фаза 19 — Семантический поиск по коду и Протокольные возможности MCP (Оценка: 2 дня)

Цель: Поиск кода естественным языком, нативная поддержка ресурсов и промптов MCP.

- [ ] **19.1** Семантическое векторное индексирование узлов кода:
  - При добавлении ключевых узлов (`Struct`, `Class`, `Interface`, `Function`, `Table`) генерировать текст описания:  
    `"{node_type} {label} in {file_path}: {description}"`.
  - Батчевая генерация эмбеддингов через существующий `embedder` (Candle MiniLM 384d / API).
  - Запись векторов в поле `embedding` таблицы `graph_nodes`.
- [ ] **19.2** Гибридный поиск по коду в `project_graph_search`:
  - Модернизация инструмента `project_graph_search`:
    - Режимы: `hybrid` (FTS5 + Cosine Vector через RRF k=60) | `text` | `ast`.
    - Агент может запрашивать концепции (*«где обработка ошибок сети?»*, *«функция шифрования паролей»*) и получать релевантный код даже без совпадения точных имён символов.
- [ ] **19.3** Реализация MCP Resources (`resources/list` и `resources/read`):
  - Регистрация capabilities: `"resources": { "subscribe": false, "listChanged": false }`.
  - Динамические ресурсы:
    - `project://current/overview` — архитектурный дайджест активного проекта.
    - `project://current/god-nodes` — список ключевых хабов связности и их зависимостей.
    - `project://current/schema` — схемы БД и DDL таблиц (извлечённые из SQL-файлов).
    - `memory://context` — текущие системные знания и профиль пользователя.
  - Агенты получают доступ к архитектуре без расхода шагов вызовов инструментов.
- [ ] **19.4** Реализация MCP Prompts (`prompts/list` и `prompts/get`):
  - Регистрация capabilities: `"prompts": { "listChanged": false }`.
  - Шаблоны:
    - `explain_component(component_name)` — генерация контекстного промпта с подтягиванием графа зависимостей компонента.
    - `plan_feature(task_description)` — промпт архитектурного планирования фичи на базе графа проекта.
- [ ] **19.5** Тесты:
  - JSON-RPC вызовы `resources/list`, `resources/read`, `prompts/list`, `prompts/get`.
  - Сравнение качества выдачи `project_graph_search` (семантический vs текстовый).

---

### Фаза 20 — Графовая архитектурная аналитика и Безопасность рефакторинга (Оценка: 2 дня)

Цель: Предотвращение поломок при рефакторинге и выявление архитектурного долга.

- [ ] **20.1** Анализ радиуса изменений (Blast Radius / `project_impact`):
  - Новый MCP-инструмент: `project_impact(id?, symbol_or_path, depth?)`:
    - Рекурсивный обход обратных рёбер (`CALLS`, `IMPORTS`, `IMPLEMENTS`, `DEPENDS_ON`).
    - Определение всех функций, классов и файлов, которые напрямую или косвенно зависят от целевого компонента.
    - Классификация риска: `Low` (локальная функция без внешних связей), `Medium`, `High` (затрагивает God Node или ядровые интерфейсы).
- [ ] **20.2** Детектор циклических зависимостей (`find_circular_dependencies`):
  - Алгоритм поиска сильно связных компонент (Tarjan's SCC) в ориентированном графе проекта.
  - Выявление циклов модулей (например, `auth -> user -> auth`).
  - Включение списка циклов в `project_report` как маркеров технического долга.
- [ ] **20.3** Метрики связанности и стабильности пакетов:
  - Afferent Coupling ($C_a$) — число входящих зависимостей.
  - Efferent Coupling ($C_e$) — число исходящих зависимостей.
  - Нестабильность $I = C_e / (C_a + C_e)$: выделение компонентов, которые опасно трогать, и компонентов, требующих изоляции.
- [ ] **20.4** Тесты:
  - Синтетический граф с циклом `A -> B -> C -> A` $\rightarrow$ успешное обнаружение цикла.
  - Проверка `project_impact` на цепочке зависимостей с подтверждением глубины.

---

## 4. Итоговый контракт инструментов MCP (25 инструментов)

Существующие 24 инструмента сохраняют 100% совместимость сигнатур. Добавляется 1 новый инструмент аналитики:

1. `memory_save(content, key?, category?, importance?, project_id?, source?)` *(project_id опционален, автодетект)*
2. `memory_search(query, limit?, mode?, project_id?)` *(project_id опционален, автодетект)*
3. `memory_update(key, content?, importance?, category?, project_id?)`
4. `memory_forget(key)`
5. `memory_context(query?, max_tokens?, project_id?)` *(project_id опционален, автодетект)*
6. `workspace_read(file)`
7. `workspace_write(file, content, commit_message)`
8. `session_log(user_text, assistant_text, project_id?, source?)` *(project_id опционален, автодетект)*
9. `knowledge_extract(text?, file_path?, max_chunks?, project_id?)`
10. `graph_search(query, limit?, project_id?, provenance?)`
11. `graph_reason(query, project_id?)`
12. `graph_stats(project_id?)`
13. `dream_run(background?)`
14. `dream_status()`
15. `dream_log(limit?)`
16. `dream_restore(commit)`
17. `omnes_stats()`
18. `omnes_backup()`
19. `session_ingest(messages, session_id?, project_id?, source?)`
20. `project_init(id, name, path, description?, tech_stack?)`
21. `project_scan(id?, path?, incremental?)` *(id опционален, автодетект; incremental=true по умолчанию)*
22. `project_context(id?, query?)` *(id опционален, автодетект)*
23. `project_graph_search(query, id?, limit?, provenance?, mode?)` *(id опционален, автодетект; добавлен режим mode=hybrid)*
24. `project_report(id?)` *(id опционален, автодетект)*
25. **`project_impact(symbol_or_path, id?, depth?)`** *(НОВЫЙ: анализ радиуса поражения изменений)*

---

## 5. Definition of Done (Критерии завершения v1.2)

- [ ] Все юнит- и интеграционные тесты Rust проходят (`cargo test`), число тестов $\ge 50$.
- [ ] Линтер `cargo clippy --all-targets` проходит без предупреждений.
- [ ] При запуске `ob2h serve` в любом репозитории агент сразу видит контекст проекта без ручного `project_init`.
- [ ] Повторный скан проекта на 1 000+ файлов занимает меньше 30 мс благодаря `project_files` SHA256-кэшу.
- [ ] Корректно отрабатывают правила `.gitignore` без замусоривания графа артефактами сборки.
- [ ] Работает фоновый File Watcher и обновляет граф при сохранении файлов.
- [ ] Команда `ob2h doctor` успешно валидирует окружение и находит все поддерживаемые агенты.
- [ ] Документация (`README.md`, `CHANGELOG.md`, `docs/ARCHITECTURE.md`) обновлена.
