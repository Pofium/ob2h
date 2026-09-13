//! 33 инструмента MCP (память, воркспейс, сессии, граф, дриминг, бэкапы, проекты, Ralph).

use super::protocol::McpToolDef;

/// Ф35.3: MCP tool annotations — подсказки harness'ам (контракт аргументов не трогается).
/// `readOnlyHint` — инструмент ничего не меняет; `destructiveHint` — может затирать данные.
/// Прочие (пишущие, но не разрушительные) остаются без аннотаций — «неизвестно» честнее.
pub fn annotations_for(name: &str) -> Option<serde_json::Value> {
    const READ_ONLY: &[&str] = &[
        "memory_search",
        "memory_context",
        "graph_search",
        "graph_reason",
        "graph_stats",
        "omnes_stats",
        "ast_history",
        "dream_log",
        "dream_status",
        "project_context",
        "project_graph_search",
        "project_impact",
        "project_report",
        "project_call_path",
        "ralph_report",
        "workspace_read",
    ];
    const DESTRUCTIVE: &[&str] = &["memory_forget", "dream_restore"];

    if READ_ONLY.contains(&name) {
        return Some(serde_json::json!({ "readOnlyHint": true, "destructiveHint": false }));
    }
    if DESTRUCTIVE.contains(&name) {
        return Some(serde_json::json!({ "readOnlyHint": false, "destructiveHint": true }));
    }
    None
}

pub fn list_tools() -> Vec<McpToolDef> {
    vec![
        // 1. memory_save
        McpToolDef {
            name: "memory_save".to_string(),
            description: "Сохранить факт в долгосрочную память. key опционален (сгенерируется). importance 0..1 — насколько важно помнить. category — произвольная метка. project_id — привязка к проекту (опционально).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Содержание факта" },
                    "key": { "type": "string", "description": "Стабильный ключ дедупликации (опционально)" },
                    "category": { "type": "string", "description": "Категория (дефолт: general)" },
                    "importance": { "type": "number", "description": "Важность от 0.0 до 1.0 (дефолт: 0.5)" },
                    "source": { "type": "string", "description": "Источник (chat|dream|extract|manual)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" }
                },
                "required": ["content"]
            }),
        },
        // 2. memory_search
        McpToolDef {
            name: "memory_search".to_string(),
            description: "Поиск по памяти: hybrid (по умолчанию, FTS+вектор RRF) | fts | vector | graph (PPR-расширение по memory_links, Ф33). project_id фильтрует по проекту. related=true добавляет соседей отдельным блоком: [related] — 1-hop по автосвязям, [ppr] — Personalized PageRank (mode=graph, fallback на 1-hop).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Поисковый запрос" },
                    "limit": { "type": "integer", "description": "Количество результатов (дефолт: 5)" },
                    "mode": { "type": "string", "enum": ["hybrid", "fts", "vector", "graph"], "description": "Режим поиска; graph — гибридные хиты + PPR-расширение (Ф33)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта для фильтрации (опционально)" },
                    "related": { "type": "boolean", "description": "Добавить связанные записи (1-hop по memory_links; при mode=graph — PPR-подграф), дефолт: false" }
                },
                "required": ["query"]
            }),
        },
        // 3. memory_update
        McpToolDef {
            name: "memory_update".to_string(),
            description: "Обновить воспоминание по ключу (любое из полей).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Ключ воспоминания" },
                    "content": { "type": "string", "description": "Новый текст" },
                    "importance": { "type": "number", "description": "Новая важность" },
                    "category": { "type": "string", "description": "Новая категория" },
                    "project_id": { "type": "string", "description": "Новый проект (опционально)" }
                },
                "required": ["key"]
            }),
        },
        // 4. memory_forget
        McpToolDef {
            name: "memory_forget".to_string(),
            description: "Удалить воспоминание по ключу.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Ключ воспоминания" }
                },
                "required": ["key"]
            }),
        },
        // 5. memory_context
        McpToolDef {
            name: "memory_context".to_string(),
            description: "Блок <agent_memory> с самыми важными фактами — для вставки в промпт. query повышает релевантность отбора. project_id фильтрует контекст проекта. author исключает записи с чужим meta.author.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Контекстный запрос" },
                    "max_tokens": { "type": "integer", "description": "Максимальный объем токенов" },
                    "max_chars": { "type": "integer", "description": "Бюджет символов блока (дефолт: OB2H_PREFETCH_MAX_CHARS=8000)" },
                    "author": { "type": "string", "description": "Автор хода: записи с чужим meta.author исключаются (Ф25.2)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" }
                }
            }),
        },
        // 6. workspace_read
        McpToolDef {
            name: "workspace_read".to_string(),
            description: "Прочитать файл агента: memory (MEMORY.md) | soul (SOUL.md) | user (USER.md) | history (консолидированная история, jsonl).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Имя файла (memory|soul|user|history)" }
                },
                "required": ["file"]
            }),
        },
        // 7. workspace_write
        McpToolDef {
            name: "workspace_write".to_string(),
            description: "Перезаписать файл агента (memory|soul|user) с git-коммитом.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Имя файла (memory|soul|user)" },
                    "content": { "type": "string", "description": "Новое содержимое файла" },
                    "commit_message": { "type": "string", "description": "Сообщение для git-коммита" }
                },
                "required": ["file", "content"]
            }),
        },
        // 8. session_log
        McpToolDef {
            name: "session_log".to_string(),
            description: "Залогировать ход диалога после ответа агента. Пишет событие в daily-лог (пища для дрима) и при переполнении бюджета токенов консолидирует итог в history.jsonl.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "user_text": { "type": "string", "description": "Сообщение пользователя" },
                    "assistant_text": { "type": "string", "description": "Ответ ассистента" },
                    "source": { "type": "string", "description": "Источник сессии (дефолт: hermes)" },
                    "author": { "type": "string", "description": "Автор хода (turn_author) — пишется в meta записи (Ф25.2)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" }
                },
                "required": ["user_text", "assistant_text"]
            }),
        },
        // 9. knowledge_extract
        McpToolDef {
            name: "knowledge_extract".to_string(),
            description: "Извлечь сущности и отношения из текста или файла (txt/md/pdf/docx) в граф знаний. Один из аргументов text/file_path обязателен.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Текст для анализа" },
                    "file_path": { "type": "string", "description": "Путь к файлу" },
                    "max_chunks": { "type": "integer", "description": "Максимум чанков (дефолт: 200)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" }
                }
            }),
        },
        // 10. graph_search
        McpToolDef {
            name: "graph_search".to_string(),
            description: "Поиск по графу знаний: узлы и связи (с 1-hop соседями). mode=ppr — Personalized PageRank по графу проекта (multi-hop, dual-seed, веса по типу рёбер). project_id и provenance позволяют точечно фильтровать.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Поисковый запрос по графу" },
                    "limit": { "type": "integer", "description": "Лимит узлов (дефолт: 10)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" },
                    "provenance": { "type": "string", "enum": ["ast", "llm", "manual", "all"], "description": "Тип источника связей (дефолт: all)" },
                    "mode": { "type": "string", "enum": ["classic", "ppr"], "description": "classic (дефолт, 1-hop) | ppr (Personalized PageRank, Ф29.2)" }
                },
                "required": ["query"]
            }),
        },
        // 11. graph_reason
        McpToolDef {
            name: "graph_reason".to_string(),
            description: "Ответ по графу знаний с уверенностью и цепочкой рассуждения (KAG). scope=docs|memory|all: memory — PPR-подграф по памяти (Ф33), all — граф знаний + память; без scope — только граф знаний (совместимость).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Вопрос к графу знаний" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" },
                    "scope": { "type": "string", "enum": ["docs", "memory", "all"], "description": "Область ответа: docs (граф знаний, дефолт) | memory (PPR по памяти) | all (оба)" }
                },
                "required": ["query"]
            }),
        },
        // 12. graph_stats
        McpToolDef {
            name: "graph_stats".to_string(),
            description: "Статистика графа знаний: узлы, связи, документы, чанки.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" }
                }
            }),
        },
        // 13. dream_run
        McpToolDef {
            name: "dream_run".to_string(),
            description: "Запустить дрим: анализ новой истории и правки MEMORY/SOUL/USER с git-коммитом. background=false ждёт завершения.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "background": { "type": "boolean", "description": "Запустить в фоновом режиме (дефолт: false)" }
                }
            }),
        },
        // 14. dream_status
        McpToolDef {
            name: "dream_status".to_string(),
            description: "Статус дрима: последний запуск, состояние гейтов автодрима.".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        },
        // 15. dream_log
        McpToolDef {
            name: "dream_log".to_string(),
            description: "История dream-коммитов в git-репозитории workspace.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Лимит записей (дефолт: 10)" }
                }
            }),
        },
        // 16. dream_restore
        McpToolDef {
            name: "dream_restore".to_string(),
            description: "Откатить MEMORY/SOUL/USER к состоянию коммита (sha из dream_log).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "commit": { "type": "string", "description": "SHA коммита" }
                },
                "required": ["commit"]
            }),
        },
        // 17. omnes_stats
        McpToolDef {
            name: "omnes_stats".to_string(),
            description: "Статистика хранилища: памяти, графа, документов, дримов.".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        },
        // 18. omnes_backup
        McpToolDef {
            name: "omnes_backup".to_string(),
            description: "Создать бэкап БД (VACUUM INTO) + workspace в backups/. Ротация 14 копий.".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        },
        // 19. session_ingest
        McpToolDef {
            name: "session_ingest".to_string(),
            description: "Массово записать транскрипту сессии (пары user/assistant) в daily-лог — пища для дрима и консолидации. При повторном вызове с тем же session_id добавляются только новые сообщения (дедуп по позиции); роли кроме user/assistant пропускаются.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "messages": {
                        "type": "array",
                        "description": "Сообщения сессии: [{role: user|assistant, content: str}]",
                        "items": {
                            "type": "object",
                            "properties": {
                                "role": { "type": "string", "enum": ["user", "assistant"] },
                                "content": { "type": "string" }
                            },
                            "required": ["role", "content"]
                        }
                    },
                    "source": { "type": "string", "description": "Источник (дефолт: hermes; напр. pre_compress)" },
                    "session_id": { "type": "string", "description": "Идентификатор сессии для дедупа (опционально)" },
                    "author": { "type": "string", "description": "Автор хода (turn_author) — пишется в meta записей (Ф25.2)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" }
                },
                "required": ["messages"]
            }),
        },
        // 20. project_init
        McpToolDef {
            name: "project_init".to_string(),
            description: "Зарегистрировать или обновить проект в памяти OB2H с привязкой к локальному пути кодовой базы.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Уникальный ID проекта (например 'ob2h', 'my-web-app')" },
                    "name": { "type": "string", "description": "Человекопонятное название проекта" },
                    "path": { "type": "string", "description": "Абсолютный или относительный путь к каталогу репозитория" },
                    "description": { "type": "string", "description": "Краткое описание назначения проекта" },
                    "tech_stack": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Список ключевых технологий (например ['rust', 'sqlite', 'mcp'])"
                    }
                },
                "required": ["id", "name", "path"]
            }),
        },
        // 21. project_scan
        McpToolDef {
            name: "project_scan".to_string(),
            description: "Запустить детерминированное статическое AST-сканирование кодовой базы проекта (без расхода LLM-токенов). Извлекает модули, функции, классы, структуры, таблицы и связи.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Идентификатор зарегистрированного проекта" },
                    "path": { "type": "string", "description": "Кастомный путь сканирования (опционально)" },
                    "incremental": { "type": "boolean", "description": "Инкрементальное обновление по SHA256 хэшам (дефолт: true)" }
                },
                "required": ["id"]
            }),
        },
        // 22. project_context
        McpToolDef {
            name: "project_context".to_string(),
            description: "Сформировать сжатый блок <project_context> для промпта агента: архитектурные хабы (God Nodes), релевантные подсистемы под задачу и метаданные.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Идентификатор проекта" },
                    "query": { "type": "string", "description": "Описание текущей задачи для точечного подбора модулей (опционально)" },
                    "mode": { "type": "string", "enum": ["context", "repo_map"], "description": "Формат: context (дефолт — прежний блок God Nodes/подсистем) или repo_map (карта «файл → символы» под token budget, Ф37)" },
                    "max_tokens": { "type": "integer", "description": "Бюджет repo_map в токенах (дефолт 4096; типовые 2048/4096/8192)" }
                },
                "required": ["id"]
            }),
        },
        // 23. project_graph_search
        McpToolDef {
            name: "project_graph_search".to_string(),
            description: "Гибридный семантический поиск по кодовому графу и символам проекта (естественным языком или точными именами функций/структур).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Идентификатор проекта" },
                    "query": { "type": "string", "description": "Поисковый запрос (имя структуры/функции или описание естественным языком)" },
                    "limit": { "type": "integer", "description": "Лимит результатов (дефолт: 15)" },
                    "provenance": { "type": "string", "enum": ["ast", "llm", "all"], "description": "Фильтр источника связей (дефолт: all)" },
                    "mode": { "type": "string", "enum": ["hybrid", "text", "vector", "callers", "callees"], "description": "Режим: hybrid (дефолт, RRF k=60), text (лексический), vector (семантический), callers/callees (структурные соседи символа из query, Ф36)" }
                },
                "required": ["id", "query"]
            }),
        },
        // 35. project_call_path (Ф36.1, трек C)
        McpToolDef {
            name: "project_call_path".to_string(),
            description: "Структурные запросы по кодовому графу (Ф36): явная цепочка вызовов/зависимостей от символа к символу (BFS по CALLS/IMPORTS/IMPLEMENTS/DEPENDS_ON); без to_symbol — кто вызывает from (mode=callers) или кого вызывает сам from (mode=callees) на глубину depth.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Идентификатор проекта (опционально — берётся активный)" },
                    "from_symbol": { "type": "string", "description": "Имя символа или путь файла-источника" },
                    "to_symbol": { "type": "string", "description": "Целевой символ/файл: если задан — ищется явная цепочка from→to" },
                    "depth": { "type": "integer", "description": "Глубина обхода (дефолт: 3, max 12 для пути / 10 для соседей)" },
                    "mode": { "type": "string", "enum": ["callers", "callees"], "description": "Без to_symbol: callers — кто вызывает from, callees — кого вызывает from (дефолт: callees)" },
                    "limit": { "type": "integer", "description": "Лимит соседей (дефолт: 25)" }
                },
                "required": ["from_symbol"]
            }),
        },
        // 24. project_report (после вставки project_call_path — №35)
        McpToolDef {
            name: "project_report".to_string(),
            description: "Сгенерировать архитектурный дайджест проекта: ключевые хабы (God Nodes), компоненты, наиболее используемые зависимости.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Идентификатор проекта" }
                },
                "required": ["id"]
            }),
        },
        // 25. project_impact
        McpToolDef {
            name: "project_impact".to_string(),
            description: "Анализ радиуса изменений (Blast Radius): находит все функции, структуры, классы и файлы, зависящие от целевого символа или файла, и оценивает риск рефакторинга.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol_or_path": { "type": "string", "description": "Имя функции, структуры, класса, интерфейса или путь к файлу проекта" },
                    "id": { "type": "string", "description": "Идентификатор проекта (опционально, по умолчанию активный проект)" },
                    "depth": { "type": "integer", "description": "Глубина обхода обратных зависимостей (дефолт: 3, от 1 до 10)" }
                },
                "required": ["symbol_or_path"]
            }),
        },
        // 26. memory_feedback (v1.3, Фаза 23)
        McpToolDef {
            name: "memory_feedback".to_string(),
            description: "Фидбек по записи памяти: корректирует trust (доверие). helpful +0.15 | unhelpful −0.2 | outdated −0.3. Записи с trust < 0.15 помечаются candidate_for_forget (не удаляются автоматически).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Ключ воспоминания" },
                    "verdict": { "type": "string", "enum": ["helpful", "unhelpful", "outdated"], "description": "Вердикт агента по полезности записи" },
                    "note": { "type": "string", "description": "Комментарий (опционально)" }
                },
                "required": ["key", "verdict"]
            }),
        },
        // 27–33. Ralph Knowledge Layer (v1.3, Фазы 26–27; контракт — спека §5)
        McpToolDef {
            name: "ralph_start".to_string(),
            description: "Начать цикл разработки (run): один активный run на (project, feature). Спека дожна жить в openspec/changes/<slug>/ репозитория.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Идентификатор проекта" },
                    "feature_slug": { "type": "string", "description": "Слаг фичи (openspec/changes/<slug>)" },
                    "goal": { "type": "string", "description": "Цель цикла" },
                    "autonomy": { "type": "string", "description": "Уровень автономии (информационно, дефолт L1)" },
                    "max_iterations_per_task": { "type": "integer", "description": "Лимит итераций на задачу (дефолт 5)" },
                    "max_total_iterations": { "type": "integer", "description": "Общий лимит итераций (дефолт 60)" },
                    "budget_tokens": { "type": "integer", "description": "Токенный бюджет (опционально)" }
                },
                "required": ["project_id", "feature_slug", "goal"]
            }),
        },
        McpToolDef {
            name: "ralph_iteration".to_string(),
            description: "Записать итерацию цикла: гипотеза/план/результат/tests_summary → авто-вердикт (только по объективным сигналам тестов, ADR-K4), AST-рескан с дельтой и staleness-pass. Идемпотентно по (run, task, n).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "ID цикла" },
                    "task_id": { "type": "string", "description": "ID задачи (T-003 из tasks.md)" },
                    "n": { "type": "integer", "description": "Номер итерации в рамках задачи" },
                    "hypothesis": { "type": "string", "description": "Размышление: почему не работало / как решаем" },
                    "plan": { "type": "string", "description": "JSON: шаги" },
                    "result": { "type": "string", "description": "JSON: итог, ошибки (self_assessment НЕ влияет на вердикт)" },
                    "tests_summary": { "type": "string", "description": "JSON: {passed, failed, fingerprint}" },
                    "ladder_rung": { "type": "string", "description": "Ступень лестницы минимальности (reuse|stdlib|platform|dep|one-line|minimal)" },
                    "git_before": { "type": "string", "description": "SHA до итерации (опционально)" },
                    "git_after": { "type": "string", "description": "SHA после итерации (опционально)" },
                    "findings": {
                        "type": "array",
                        "description": "Findings итерации: [{kind: hypothesis|gotcha|decision|constraint|deferred, content, symbols?: [\"fn:name\"], meta?: {...}}]; deferred = Ponytail-маркер {ceiling, upgrade_trigger}",
                        "items": { "type": "object", "properties": { "kind": { "type": "string" }, "content": { "type": "string" }, "symbols": { "type": "array", "items": { "type": "string" } }, "meta": { "type": "object" } }, "required": ["kind", "content"] }
                    }
                },
                "required": ["run_id", "task_id", "n"]
            }),
        },
        McpToolDef {
            name: "ralph_verdict".to_string(),
            description: "Сменить вердикт итерации или finding'а (человек/дрим/агент): verified|failed|unconfirmed|overturned|stale.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "iteration_id": { "type": "string", "description": "ID итерации (или finding_id)" },
                    "finding_id": { "type": "string", "description": "ID finding'а (или iteration_id)" },
                    "verdict": { "type": "string", "enum": ["verified", "failed", "unconfirmed", "overturned", "stale"] },
                    "verdict_source": { "type": "string", "description": "Кто поставил: auto_tests|auto_verify|human|dream" }
                }
            }),
        },
        McpToolDef {
            name: "ralph_context".to_string(),
            description: "Контекст-пакет на задачу цикла: фрагмент спеки → негативный опыт → reuse-кандидаты из AST-графа → инварианты → god nodes. mode=lite — короче, без архитектурной зоны.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "ID цикла" },
                    "task_id": { "type": "string", "description": "ID задачи" },
                    "max_tokens": { "type": "integer", "description": "Бюджет пакета в токенах (дефолт 6000)" },
                    "mode": { "type": "string", "enum": ["lite", "full"], "description": "Режим пакета (дефолт full)" }
                },
                "required": ["run_id", "task_id"]
            }),
        },
        McpToolDef {
            name: "ralph_report".to_string(),
            description: "Сводка цикла(ов): прогресс, вердикты, debt-леджер (deferred/no-trigger), gain-метрики (reuse-hit rate).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "ID цикла (опционально — все)" }
                }
            }),
        },
        McpToolDef {
            name: "ast_diff".to_string(),
            description: "Symbol-level дифф кода между итерациями цикла (по AST-дельтам, не git-дифф).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Идентификатор проекта" },
                    "from": { "type": "string", "description": "iteration_id | commit | timestamp" },
                    "to": { "type": "string", "description": "до (опционально, дефолт: сейчас)" }
                },
                "required": ["project_id", "from"]
            }),
        },
        McpToolDef {
            name: "ast_history".to_string(),
            description: "Хронология изменений символа по итерациям цикла.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Идентификатор проекта" },
                    "symbol": { "type": "string", "description": "Имя символа (функция/структура/...)" }
                },
                "required": ["project_id", "symbol"]
            }),
        },
        // 34. memory_merge (v1.4, Фаза 31) — явное подтверждённое слияние
        McpToolDef {
            name: "memory_merge".to_string(),
            description: "Явное подтверждённое слияние почти-дублей памяти: каноническая запись получает union meta / max importance / sum access, остальные — tombstone с meta.merged_into, links перенаправляются. Авто-слияние запрещено — только вызов агента или дрим-вердикт.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "keys": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Сливаемые ключи (≥2, все живые)"
                    },
                    "canonical_key": { "type": "string", "description": "Канонический ключ (дефолт: максимум trust/importance)" },
                    "note": { "type": "string", "description": "Причина слияния — попадает в meta обеих сторон" }
                },
                "required": ["keys"]
            }),
        },
    ]
}
