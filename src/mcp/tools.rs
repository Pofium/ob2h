//! 24 инструмента MCP (память, воркспейс, сессии, граф, дриминг, бэкапы, проекты).

use super::protocol::McpToolDef;

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
            description: "Поиск по памяти: hybrid (по умолчанию, FTS+вектор RRF) | fts | vector. project_id фильтрует по проекту.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Поисковый запрос" },
                    "limit": { "type": "integer", "description": "Количество результатов (дефолт: 5)" },
                    "mode": { "type": "string", "enum": ["hybrid", "fts", "vector"], "description": "Режим поиска" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта для фильтрации (опционально)" }
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
            description: "Блок <agent_memory> с самыми важными фактами — для вставки в промпт. query повышает релевантность отбора. project_id фильтрует контекст проекта.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Контекстный запрос" },
                    "max_tokens": { "type": "integer", "description": "Максимальный объем токенов" },
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
            description: "Поиск по графу знаний: узлы и связи (с 1-hop соседями). project_id и provenance позволяют точечно фильтровать.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Поисковый запрос по графу" },
                    "limit": { "type": "integer", "description": "Лимит узлов (дефолт: 10)" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" },
                    "provenance": { "type": "string", "enum": ["ast", "llm", "manual", "all"], "description": "Тип источника связей (дефолт: all)" }
                },
                "required": ["query"]
            }),
        },
        // 11. graph_reason
        McpToolDef {
            name: "graph_reason".to_string(),
            description: "Ответ по графу знаний с уверенностью и цепочкой рассуждения (KAG).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Вопрос к графу знаний" },
                    "project_id": { "type": "string", "description": "Идентификатор проекта (опционально)" }
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
                    "query": { "type": "string", "description": "Описание текущей задачи для точечного подбора модулей (опционально)" }
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
                    "mode": { "type": "string", "enum": ["hybrid", "text", "vector"], "description": "Режим поиска: hybrid (дефолт, RRF k=60), text (лексический), vector (семантический)" }
                },
                "required": ["id", "query"]
            }),
        },
        // 24. project_report
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
    ]
}
