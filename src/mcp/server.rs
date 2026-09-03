//! MCP-сервер (stdio transport). Обработка JSON-RPC запросов и вызов инструментов.

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::info;

use super::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpContent, McpToolCallResult};
use super::tools::list_tools;
use crate::backup::BackupManager;
use crate::config::Settings;
use crate::consolidator::{PendingSession, Consolidator};
use crate::db::{utcnow, Database};
use crate::dream::Dream;
use crate::embedding::EmbeddingProvider;
use crate::extractor::{split_into_chunks, Extractor};
use crate::graph::GraphService;
use crate::ingest::read_document;
use crate::llm::LLMClient;
use crate::memory::MemoryService;
use crate::project::ProjectService;
use crate::vector::serialize;
use crate::workspace::{GitStore, Workspace};

pub struct AppContext {
    pub settings: Settings,
    pub db: Database,
    pub embedder: Arc<dyn EmbeddingProvider>,
    pub memory: Arc<MemoryService>,
    pub workspace: Arc<Workspace>,
    pub gitstore: Arc<GitStore>,
    pub llm: Arc<dyn LLMClient>,
    pub consolidator: Arc<Consolidator>,
    pub pending_session: Arc<Mutex<PendingSession>>,
    pub graph: Arc<GraphService>,
    pub project: Arc<ProjectService>,
    pub dream: Arc<Dream>,
    pub backup: Arc<BackupManager>,
    pub sync: Arc<crate::sync::SyncManager>,
    pub dream_lock: Arc<Mutex<()>>,
    pub active_workspace: Arc<tokio::sync::RwLock<Option<std::path::PathBuf>>>,
    pub active_project_id: Arc<tokio::sync::RwLock<Option<String>>>,
    pub watcher: Arc<crate::project::ProjectWatcher>,
}

pub struct McpServer {
    ctx: Arc<AppContext>,
}

fn parse_file_uri(uri: &str) -> Option<std::path::PathBuf> {
    let s = uri.trim();
    let stripped = if let Some(p) = s.strip_prefix("file:///") {
        p
    } else {
        s.strip_prefix("file://")?
    };

    let mut res = String::new();
    let mut chars = stripped.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(c1), Some(c2)) = (h1, h2) {
                let hex_str = format!("{}{}", c1, c2);
                if let Ok(b) = u8::from_str_radix(&hex_str, 16) {
                    res.push(b as char);
                    continue;
                }
                res.push('%');
                res.push(c1);
                res.push(c2);
            } else {
                res.push('%');
                if let Some(c1) = h1 { res.push(c1); }
            }
        } else {
            res.push(c);
        }
    }

    Some(std::path::PathBuf::from(res))
}

impl McpServer {
    pub fn new(ctx: Arc<AppContext>) -> Self {
        Self { ctx }
    }

    pub fn truncate(&self, text: &str) -> String {
        let max_chars = self.ctx.settings.max_tool_output_chars;
        if text.chars().count() <= max_chars {
            text.to_string()
        } else {
            let truncated: String = text.chars().take(max_chars).collect();
            format!("{truncated}…[truncated]")
        }
    }

    pub async fn run_stdio(self: Arc<Self>) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
        let mut reader = BufReader::new(stdin).lines();

        info!("MCP Stdio Server running, waiting for JSON-RPC messages...");

        let mut tasks = tokio::task::JoinSet::new();

        while let Some(line) = reader.next_line().await? {
            let line_str = line.trim();
            if line_str.is_empty() {
                continue;
            }

            let req: JsonRpcRequest = match serde_json::from_str(line_str) {
                Ok(r) => r,
                Err(e) => {
                    let resp = JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: None,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("Parse error: {e}"),
                            data: None,
                        }),
                    };
                    let resp_str = serde_json::to_string(&resp)?;
                    let mut out = stdout.lock().await;
                    out.write_all(resp_str.as_bytes()).await?;
                    out.write_all(b"\n").await?;
                    out.flush().await?;
                    continue;
                }
            };

            // Каждый запрос обрабатываем в отдельной задаче: пока выполняется
            // долгий tools/call (knowledge_extract, dream_run), сервер продолжает
            // читать stdin и отвечает на ping — иначе keepalive-клиента (Hermes)
            // рвёт соединение («Connection closed» на вызовах >30с).
            let this = self.clone();
            let out = stdout.clone();
            tasks.spawn(async move {
                let response = this.handle_request(req).await;
                if let Some(resp) = response {
                    if let Ok(resp_str) = serde_json::to_string(&resp) {
                        let mut o = out.lock().await;
                        let _ = o.write_all(resp_str.as_bytes()).await;
                        let _ = o.write_all(b"\n").await;
                        let _ = o.flush().await;
                    }
                }
            });

            // Периодически вычищаем завершившиеся задачи, чтобы не копить.
            if tasks.len() >= 64 {
                while tasks.join_next().await.is_some() {}
            }
        }

        // stdin закрыт — дожидаемся хвостовых задач.
        while tasks.join_next().await.is_some() {}

        Ok(())
    }

    pub async fn handle_request(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let id = req.id.clone();
        match req.method.as_str() {
            "initialize" => {
                // Автоматическое определение рабочей директории сессии агента
                let mut detected_path: Option<std::path::PathBuf> = None;

                if let Some(params) = &req.params {
                    // 1. Проверяем workspaceFolders
                    if let Some(folders) = params.get("workspaceFolders").and_then(|v| v.as_array()) {
                        for f in folders {
                            if let Some(uri) = f.get("uri").and_then(|u| u.as_str()) {
                                if let Some(p) = parse_file_uri(uri) {
                                    detected_path = Some(p);
                                    break;
                                }
                            }
                        }
                    }

                    // 2. Проверяем rootUri
                    if detected_path.is_none() {
                        if let Some(uri) = params.get("rootUri").and_then(|u| u.as_str()) {
                            detected_path = parse_file_uri(uri);
                        }
                    }

                    // 3. Проверяем rootPath
                    if detected_path.is_none() {
                        if let Some(path_str) = params.get("rootPath").and_then(|p| p.as_str()) {
                            detected_path = Some(std::path::PathBuf::from(path_str));
                        }
                    }
                }

                // 4. Fallback на std::env::current_dir()
                let target_dir = detected_path.unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                });

                // Zero-Config: автодетект и регистрация проекта
                if let Ok(project) = self.ctx.project.auto_register_or_detect(&target_dir) {
                    *self.ctx.active_workspace.write().await = Some(std::path::PathBuf::from(&project.root_path));
                    *self.ctx.active_project_id.write().await = Some(project.id.clone());
                    info!("Zero-Config: MCP-сессия привязана к проекту '{}' ({})", project.name, project.id);

                    if self.ctx.settings.watcher_enabled {
                        let watcher = self.ctx.watcher.clone();
                        let pid = project.id.clone();
                        let proot = std::path::PathBuf::from(&project.root_path);
                        tokio::spawn(async move {
                            if let Err(e) = watcher.switch_project(&pid, &proot).await {
                                tracing::warn!("FileWatcher: не удалось активировать для {}: {e}", pid);
                            }
                        });
                    }
                }

                let result = serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": { "listChanged": false },
                        "resources": { "subscribe": false, "listChanged": false },
                        "prompts": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "ob2h",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                });
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(result),
                    error: None,
                })
            }
            "notifications/initialized" | "initialized" => None,
            "ping" => Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({})),
                error: None,
            }),
            "tools/list" => {
                let tools = list_tools();
                let result = serde_json::json!({ "tools": tools });
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(result),
                    error: None,
                })
            }
            "resources/list" => {
                let resources = serde_json::json!({
                    "resources": [
                        {
                            "uri": "project://current/overview",
                            "name": "Архитектурный дайджест активного проекта",
                            "description": "Сводный отчет о стеке, метриках кодовой базы и ключевых хабах связности",
                            "mimeType": "text/markdown"
                        },
                        {
                            "uri": "project://current/god-nodes",
                            "name": "Ключевые узлы связности (God Nodes)",
                            "description": "Список наиболее связных структур, классов и модулей текущего проекта",
                            "mimeType": "text/markdown"
                        },
                        {
                            "uri": "project://current/schema",
                            "name": "Схема данных проекта (Таблицы и DDL)",
                            "description": "Таблицы БД, поля и связи, извлеченные из SQL-миграций и моделей",
                            "mimeType": "text/markdown"
                        },
                        {
                            "uri": "memory://context",
                            "name": "Долговременный контекст и профиль пользователя",
                            "description": "Ключевые системные знания агента, предпочтения разработчика и важные факты",
                            "mimeType": "text/markdown"
                        }
                    ]
                });
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(resources),
                    error: None,
                })
            }
            "resources/read" => {
                let params = req.params.unwrap_or(serde_json::json!({}));
                let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                let active_proj = self.ctx.active_project_id.read().await.clone();
                let (text, mime_type) = self.read_resource(uri, active_proj.as_deref()).await;

                let result = serde_json::json!({
                    "contents": [
                        {
                            "uri": uri,
                            "mimeType": mime_type,
                            "text": text
                        }
                    ]
                });
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(result),
                    error: None,
                })
            }
            "prompts/list" => {
                let prompts = serde_json::json!({
                    "prompts": [
                        {
                            "name": "explain_component",
                            "description": "Построить детальное контекстное объяснение компонента проекта с учетом входящих и исходящих связей",
                            "arguments": [
                                {
                                    "name": "component_name",
                                    "description": "Имя функции, структуры, класса или модуля проекта",
                                    "required": true
                                }
                            ]
                        },
                        {
                            "name": "plan_feature",
                            "description": "Сгенерировать архитектурный план внедрения новой фичи с учетом техстека и ключевых хабов проекта",
                            "arguments": [
                                {
                                    "name": "task_description",
                                    "description": "Описание задачи или новой функциональности",
                                    "required": true
                                }
                            ]
                        }
                    ]
                });
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(prompts),
                    error: None,
                })
            }
            "prompts/get" => {
                let params = req.params.unwrap_or(serde_json::json!({}));
                let prompt_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                let active_proj = self.ctx.active_project_id.read().await.clone();

                let prompt_result = self.get_prompt(prompt_name, &args, active_proj.as_deref()).await;

                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(prompt_result),
                    error: None,
                })
            }
            "tools/call" => {
                let params = req.params.unwrap_or(serde_json::json!({}));
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let mut args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));

                // Автоматическая подстановка active_project_id, если аргумент не передан
                if let Some(active_id) = self.ctx.active_project_id.read().await.as_ref() {
                    if let Some(obj) = args.as_object_mut() {
                        let needs_project_id = match tool_name {
                            "memory_save" | "memory_search" | "memory_update" | "memory_context"
                            | "session_log" | "session_ingest"
                            | "knowledge_extract" | "graph_search" | "graph_reason" | "graph_stats"
                            | "project_context" | "project_graph_search" | "project_report" => true,
                            "project_scan" => !obj.contains_key("id"),
                            _ => false,
                        };

                        if needs_project_id {
                            let empty = obj.get("project_id").and_then(|v| v.as_str()).is_none_or(|s| s.is_empty());
                            if empty {
                                obj.insert("project_id".to_string(), serde_json::Value::String(active_id.clone()));
                            }
                            if tool_name == "project_scan" || tool_name == "project_context" || tool_name == "project_graph_search" || tool_name == "project_report" {
                                let id_empty = obj.get("id").and_then(|v| v.as_str()).is_none_or(|s| s.is_empty());
                                if id_empty {
                                    obj.insert("id".to_string(), serde_json::Value::String(active_id.clone()));
                                }
                            }
                        }
                    }
                }

                let output = self.call_tool(tool_name, args).await;
                let truncated_output = self.truncate(&output);

                let is_error = output.starts_with("[Error]");
                let tool_res = McpToolCallResult {
                    content: vec![McpContent {
                        content_type: "text".to_string(),
                        text: truncated_output,
                    }],
                    is_error: if is_error { Some(true) } else { None },
                };

                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(serde_json::to_value(tool_res).unwrap()),
                    error: None,
                })
            }
            other => Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {other}"),
                    data: None,
                }),
            }),
        }
    }

    pub async fn call_tool(&self, name: &str, args: serde_json::Value) -> String {
        match name {
            "memory_save" => {
                let content = match args.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return "[Error] content is required".to_string(),
                };
                let key = args.get("key").and_then(|v| v.as_str());
                let category = args.get("category").and_then(|v| v.as_str()).unwrap_or("general");
                let importance = args.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5);
                let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("chat");
                let project_id = args.get("project_id").and_then(|v| v.as_str());

                match self.ctx.memory.save_with_project(content, key, category, importance, source, None, project_id).await {
                    Ok(k) => format!("saved key={k}"),
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "memory_search" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => return "[Error] query is required".to_string(),
                };
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("hybrid");

                let hits = match mode {
                    "fts" => {
                        let res = self.ctx.memory.search_fts(query, limit).unwrap_or_default();
                        res.into_iter()
                            .filter_map(|(id, score)| {
                                self.ctx.memory.get_by_id(id).ok().flatten().map(|r| crate::memory::MemoryHit {
                                    record: r,
                                    score,
                                    match_type: "fts".to_string(),
                                })
                            })
                            .collect::<Vec<_>>()
                    }
                    "vector" => {
                        let res = self.ctx.memory.search_vector(query, limit, 0.0).await.unwrap_or_default();
                        res.into_iter()
                            .filter_map(|(id, score)| {
                                self.ctx.memory.get_by_id(id).ok().flatten().map(|r| crate::memory::MemoryHit {
                                    record: r,
                                    score,
                                    match_type: "vector".to_string(),
                                })
                            })
                            .collect::<Vec<_>>()
                    }
                    _ => self.ctx.memory.search_hybrid(query, limit, 0.0).await.unwrap_or_default(),
                };

                if hits.is_empty() {
                    return "ничего не найдено".to_string();
                }

                hits.iter()
                    .enumerate()
                    .map(|(i, h)| {
                        let preview: String = h.record.content.chars().take(200).collect();
                        format!(
                            "[{}] key={} cat={} imp={:.2} score={:.4} | {preview}",
                            i + 1,
                            h.record.key,
                            h.record.category,
                            h.record.importance,
                            h.score
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "memory_update" => {
                let key = match args.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => return "[Error] key is required".to_string(),
                };
                let content = args.get("content").and_then(|v| v.as_str());
                let importance = args.get("importance").and_then(|v| v.as_f64());
                let category = args.get("category").and_then(|v| v.as_str());

                match self.ctx.memory.update(key, content, importance, category).await {
                    Ok(true) => format!("updated key={key}"),
                    Ok(false) => format!("not found key={key}"),
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "memory_forget" => {
                let key = match args.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => return "[Error] key is required".to_string(),
                };
                match self.ctx.memory.forget(key) {
                    Ok(true) => format!("forgotten key={key}"),
                    Ok(false) => format!("not found key={key}"),
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "memory_context" => {
                let query = args.get("query").and_then(|v| v.as_str());
                let limit = args.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
                match self.ctx.memory.build_context(limit, query) {
                    Ok(ctx) => ctx,
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "workspace_read" => {
                let file = match args.get("file").and_then(|v| v.as_str()) {
                    Some(f) => f,
                    None => return "[Error] file is required".to_string(),
                };
                match self.ctx.workspace.read_file(file) {
                    Ok(content) => content,
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "workspace_write" => {
                let file = match args.get("file").and_then(|v| v.as_str()) {
                    Some(f) => f,
                    None => return "[Error] file is required".to_string(),
                };
                let content = match args.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return "[Error] content is required".to_string(),
                };
                let msg = args.get("commit_message").and_then(|v| v.as_str()).unwrap_or("");

                match self.ctx.workspace.write_file(file, content) {
                    Ok(_) => {
                        let commit_msg = if msg.is_empty() { format!("agent write: {file}") } else { msg.to_string() };
                        let sha = self.ctx.gitstore.auto_commit(&commit_msg);
                        format!("written {file}{}", sha.map(|s| format!(" commit={s}")).unwrap_or_default())
                    }
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "session_log" => {
                let user_text = match args.get("user_text").and_then(|v| v.as_str()) {
                    Some(u) => u,
                    None => return "[Error] user_text is required".to_string(),
                };
                let assistant_text = match args.get("assistant_text").and_then(|v| v.as_str()) {
                    Some(a) => a,
                    None => return "[Error] assistant_text is required".to_string(),
                };
                let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("hermes");

                let _ = self.ctx.workspace.log_daily_session(
                    user_text,
                    assistant_text,
                    Some(serde_json::json!({ "source": source })),
                );

                let mut session = self.ctx.pending_session.lock().await;
                session.append("user", user_text);
                session.append("assistant", assistant_text);

                match self.ctx.consolidator.maybe_consolidate(&mut session).await {
                    Ok(res) if res.consolidated => format!("logged +consolidated x{}", res.entries),
                    Ok(_) => "logged".to_string(),
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "session_ingest" => {
                let messages = match args.get("messages").and_then(|v| v.as_array()) {
                    Some(m) if !m.is_empty() => m,
                    _ => return "[Error] messages is required (непустой массив {role, content})".to_string(),
                };
                let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("hermes");
                let session_id = args.get("session_id").and_then(|v| v.as_str()).unwrap_or("");

                // Дедуп: kv "ingest:<session_id>" хранит, сколько сообщений этой сессии
                // уже принято (включая пропущенные роли) — повторный вызов с полной
                // транскриптой добавляет только хвост.
                let kv_key = format!("ingest:{session_id}");
                let already: usize = if session_id.is_empty() {
                    0
                } else {
                    self.ctx
                        .db
                        .get_kv(&kv_key)
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0)
                };

                let mut skipped = 0usize;
                let mut new_pairs: Vec<(String, String)> = Vec::new();
                let mut pending_user = String::new();
                let mut has_user = false;
                for (i, m) in messages.iter().enumerate() {
                    if i < already {
                        skipped += 1;
                        continue;
                    }
                    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    if role != "user" && role != "assistant" {
                        continue;
                    }
                    if role == "user" {
                        if has_user {
                            pending_user.push('\n');
                        }
                        pending_user.push_str(content);
                        has_user = true;
                    } else {
                        let user_text = if has_user { pending_user.clone() } else { String::new() };
                        new_pairs.push((user_text, content.to_string()));
                        pending_user.clear();
                        has_user = false;
                    }
                }
                // Хвостовой user без ответа не пишем — пара ещё не завершилась.

                if new_pairs.is_empty() {
                    if !session_id.is_empty() {
                        let _ = self.ctx.db.set_kv(&kv_key, &messages.len().to_string());
                    }
                    return format!("ingested pairs=0 skipped_msgs={skipped} (новых сообщений нет)");
                }

                let mut written = 0usize;
                let mut consolidated = 0usize;
                let mut last_err: Option<String> = None;
                {
                    let mut session = self.ctx.pending_session.lock().await;
                    for (u, a) in &new_pairs {
                        let _ = self.ctx.workspace.log_daily_session(
                            u,
                            a,
                            Some(serde_json::json!({ "source": source, "session_id": session_id })),
                        );
                        session.append("user", u);
                        session.append("assistant", a);
                        written += 1;
                        match self.ctx.consolidator.maybe_consolidate(&mut session).await {
                            Ok(res) if res.consolidated => consolidated += res.entries,
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("session_ingest: консолидация не удалась: {e}");
                                last_err = Some(e.to_string());
                            }
                        }
                    }
                }

                if !session_id.is_empty() {
                    let consumed = already.max(messages.len());
                    if let Err(e) = self.ctx.db.set_kv(&kv_key, &consumed.to_string()) {
                        tracing::warn!("session_ingest: не сохранить дедуп-счётчик: {e}");
                    }
                }

                let mut out = format!("ingested pairs={written} skipped_msgs={skipped}");
                if consolidated > 0 {
                    out.push_str(&format!(" +consolidated x{consolidated}"));
                }
                if let Some(e) = last_err {
                    out.push_str(&format!(" [Error] консолидация: {e}"));
                }
                out
            }
            "knowledge_extract" => {
                let text_arg = args.get("text").and_then(|v| v.as_str());
                let file_path = args.get("file_path").and_then(|v| v.as_str());
                let max_chunks = args.get("max_chunks").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

                let (text, title, meta) = if let Some(path) = file_path {
                    match read_document(path) {
                        Ok((t, m)) => {
                            let title = m.file_name.clone();
                            let meta_json = serde_json::to_string(&m).unwrap_or_default();
                            (t, title, meta_json)
                        }
                        Err(e) => return format!("[Error] {e}"),
                    }
                } else if let Some(t) = text_arg {
                    (t.to_string(), "текст из чата".to_string(), "{}".to_string())
                } else {
                    return "[Error] укажите text или file_path".to_string();
                };

                if text.trim().is_empty() {
                    return "[Error] пустой текст".to_string();
                }

                let now = utcnow();
                let doc_id: i64 = match self.ctx.db.with_conn(|conn| {
                    conn.execute(
                        "INSERT INTO documents (title, path, meta, created_at) VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![title, file_path, meta, now],
                    )?;
                    Ok(conn.last_insert_rowid())
                }) {
                    Ok(id) => id,
                    Err(e) => return format!("[Error] {e}"),
                };

                let chunks = split_into_chunks(&text, 3000, 300);
                let limited_chunks: Vec<String> = chunks.into_iter().take(max_chunks).collect();
                let chunk_vecs = self.ctx.embedder.embed(&limited_chunks).await.unwrap_or_default();

                let _ = self.ctx.db.with_conn(|conn| {
                    for (ord, (chunk, vec)) in limited_chunks.iter().zip(chunk_vecs.iter()).enumerate() {
                        let blob = serialize(vec);
                        let _ = conn.execute(
                            "INSERT INTO chunks (doc_id, ordinal, text, embedding, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                            rusqlite::params![doc_id, ord as i64, chunk, blob, now],
                        );
                    }
                    Ok(())
                });

                let extractor = Extractor::new(self.ctx.llm.clone(), max_chunks);
                match extractor.extract(&text).await {
                    Ok(result) => {
                        let (new_nodes, updated_nodes, new_edges) = self.ctx.graph.upsert_extraction(&result).await.unwrap_or_default();
                        format!(
                            "doc_id={doc_id} chunks={}(+{} пропущено) entities={}новых+{}дублей relations={}новых",
                            result.chunks_processed, result.chunks_skipped, new_nodes, updated_nodes, new_edges
                        )
                    }
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "graph_search" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => return "[Error] query is required".to_string(),
                };
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

                match self.ctx.graph.search(query, limit, true).await {
                    Ok(found) => {
                        if found.nodes.is_empty() {
                            return "граф пуст по запросу".to_string();
                        }
                        let mut lines = vec![format!("узлов: {}, связей: {}", found.nodes.len(), found.edges.len())];
                        for n in found.nodes.iter().take(limit) {
                            let desc = n.description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default();
                            lines.push(format!("- {} ({}, val={}){}", n.label, n.node_type, n.val, desc));
                        }
                        for e in found.edges.iter().take(limit * 2) {
                            lines.push(format!("- {} --[{}]--> {}", e.source_label, e.label, e.target_label));
                        }
                        lines.join("\n")
                    }
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "graph_reason" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => return "[Error] query is required".to_string(),
                };

                match self.ctx.graph.reason(query, self.ctx.llm.clone()).await {
                    Ok(res) => {
                        let steps = res.reasoning_steps.join("; ");
                        let entities = res.used_entities.join(", ");
                        format!(
                            "answer: {}\nconfidence: {}\nentities: {}\nsteps: {}",
                            res.answer,
                            res.confidence,
                            if entities.is_empty() { "-" } else { &entities },
                            if steps.is_empty() { "-" } else { &steps }
                        )
                    }
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "graph_stats" => match self.ctx.graph.stats() {
                Ok(s) => format!("nodes={} edges={} documents={} chunks={}", s.nodes, s.edges, s.documents, s.chunks),
                Err(e) => format!("[Error] {e}"),
            },
            "dream_run" => {
                let background = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
                let lock = self.ctx.dream_lock.clone();

                if background {
                    let dream = self.ctx.dream.clone();
                    tokio::spawn(async move {
                        let _guard = lock.lock().await;
                        let _ = dream.run("manual-bg").await;
                    });
                    "дрим запущен в фоне — статус: dream_status".to_string()
                } else {
                    let _guard = lock.lock().await;
                    match self.ctx.dream.run("manual").await {
                        Ok(res) => {
                            if res.status == "error" {
                                format!("[Error] {}", res.error.unwrap_or_default())
                            } else {
                                format!(
                                    "run_id={} processed={} edits={} commit={}{}",
                                    res.run_id,
                                    res.processed,
                                    res.edits,
                                    res.commit.unwrap_or_else(|| "-".to_string()),
                                    res.note.map(|n| format!(" ({n})")).unwrap_or_default()
                                )
                            }
                        }
                        Err(e) => format!("[Error] {e}"),
                    }
                }
            }
            "dream_status" => match self.ctx.dream.last_status() {
                Ok(Some(status)) => serde_json::to_string_pretty(&status).unwrap_or_default(),
                Ok(None) => "last_run: никогда\ndream_cursor: none".to_string(),
                Err(e) => format!("[Error] {e}"),
            },
            "dream_log" => {
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let entries = self.ctx.gitstore.log(limit);
                if entries.is_empty() {
                    return "коммитов нет".to_string();
                }
                entries
                    .into_iter()
                    .map(|e| format!("{} {} {}", e.sha, e.date, e.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "dream_restore" => {
                let commit = match args.get("commit").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return "[Error] commit is required".to_string(),
                };
                self.ctx.gitstore.restore(commit)
            }
            "omnes_stats" => {
                let db_size = self.ctx.settings.db_path().metadata().map(|m| m.len()).unwrap_or(0);
                let counts = self.ctx.db.with_conn(|conn| {
                    let m: i64 = conn.query_row("SELECT count(*) FROM memories", [], |r| r.get(0)).unwrap_or(0);
                    let r: i64 = conn.query_row("SELECT count(*) FROM memory_relations", [], |r| r.get(0)).unwrap_or(0);
                    let d: i64 = conn.query_row("SELECT count(*) FROM documents", [], |r| r.get(0)).unwrap_or(0);
                    let c: i64 = conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0)).unwrap_or(0);
                    let gn: i64 = conn.query_row("SELECT count(*) FROM graph_nodes", [], |r| r.get(0)).unwrap_or(0);
                    let ge: i64 = conn.query_row("SELECT count(*) FROM graph_edges", [], |r| r.get(0)).unwrap_or(0);
                    let dr: i64 = conn.query_row("SELECT count(*) FROM dream_runs", [], |r| r.get(0)).unwrap_or(0);
                    Ok((m, r, d, c, gn, ge, dr))
                }).unwrap_or_default();

                format!(
                    "memories={} relations={} documents={} chunks={} graph_nodes={} graph_edges={} dream_runs={} db={}KB",
                    counts.0, counts.1, counts.2, counts.3, counts.4, counts.5, counts.6, db_size / 1024
                )
            }
            "omnes_backup" => match self.ctx.backup.create() {
                Ok(path) => format!("backup: {}", path.display()),
                Err(e) => format!("[Error] {e}"),
            },
            "project_init" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(i) => i,
                    None => return "[Error] id is required".to_string(),
                };
                let name = match args.get("name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return "[Error] name is required".to_string(),
                };
                let path = match args.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => return "[Error] path is required".to_string(),
                };
                let desc = args.get("description").and_then(|v| v.as_str());
                let tech_stack: Option<Vec<String>> = args.get("tech_stack").and_then(|v| {
                    v.as_array().map(|arr| {
                        arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()
                    })
                });

                match self.ctx.project.register_project(id, name, path, desc, tech_stack.as_deref()) {
                    Ok(p) => format!("project registered: id={} name='{}' root='{}'", p.id, p.name, p.root_path),
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "project_scan" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(i) => i,
                    None => return "[Error] id is required".to_string(),
                };
                let path = args.get("path").and_then(|v| v.as_str());
                let incremental = args.get("incremental").and_then(|v| v.as_bool()).unwrap_or(true);

                match self.ctx.project.scan_project(id, path, incremental) {
                    Ok(res) => {
                        let _ = self.ctx.db.with_conn(|conn| {
                            let _ = crate::graph::GraphAnalytics::update_god_nodes(conn, id);
                            Ok(())
                        });
                        let _ = self.ctx.project.embed_unembedded_nodes(id).await;
                        format!(
                            "project '{}' scanned: files={} nodes={} edges={} total_lines={}",
                            id, res.files_scanned, res.nodes.len(), res.edges.len(), res.lines_total
                        )
                    }
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "project_context" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(i) => i,
                    None => return "[Error] id is required".to_string(),
                };
                let query = args.get("query").and_then(|v| v.as_str());

                match self.ctx.db.with_conn(|conn| crate::graph::GraphAnalytics::build_project_context(conn, id, query)) {
                    Ok(ctx) => ctx,
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "project_graph_search" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(i) => i,
                    None => return "[Error] id is required".to_string(),
                };
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => return "[Error] query is required".to_string(),
                };
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(15) as usize;
                let provenance = args.get("provenance").and_then(|v| v.as_str()).unwrap_or("all");
                let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("hybrid");

                match self.ctx.graph.search_project_nodes_hybrid(id, query, mode, provenance, limit).await {
                    Ok(nodes) => {
                        if nodes.is_empty() {
                            "совпадений в графе проекта не найдено".to_string()
                        } else {
                            let mut lines = Vec::new();
                            for n in nodes {
                                let loc = if let (Some(f), Some(l)) = (&n.file_path, n.line_start) {
                                    format!("{}:{}", f, l)
                                } else {
                                    "-".to_string()
                                };
                                let god_mark = if n.is_god_node { " [👑 GodNode]" } else { "" };
                                lines.push(format!(
                                    "- `{}` [{}] ({}) score={:.4} prov={}{}: {}",
                                    n.label, n.node_type, loc, n.score,
                                    n.provenance.as_deref().unwrap_or("ast"),
                                    god_mark, n.description.as_deref().unwrap_or("")
                                ));
                            }
                            lines.join("\n")
                        }
                    }
                    Err(e) => format!("[Error] {e}"),
                }
            }
            "project_report" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(i) => i,
                    None => return "[Error] id is required".to_string(),
                };

                match self.ctx.db.with_conn(|conn| crate::graph::GraphAnalytics::generate_project_report(conn, id)) {
                    Ok(rep) => rep.markdown_summary,
                    Err(e) => format!("[Error] {e}"),
                }
            }
            other => format!("[Error] Unknown tool {other}"),
        }
    }

    async fn read_resource(&self, uri: &str, active_proj: Option<&str>) -> (String, &'static str) {
        match uri {
            "project://current/overview" => {
                if let Some(pid) = active_proj {
                    match self.ctx.db.with_conn(|conn| crate::graph::GraphAnalytics::generate_project_report(conn, pid)) {
                        Ok(rep) => (rep.markdown_summary, "text/markdown"),
                        Err(e) => (format!("Ошибка генерации отчета: {e}"), "text/markdown"),
                    }
                } else {
                    ("Активный проект не привязан к сессии.".to_string(), "text/markdown")
                }
            }
            "project://current/god-nodes" => {
                if let Some(pid) = active_proj {
                    let text = self.ctx.db.with_conn(|conn| {
                        let mut stmt = conn.prepare(
                            r#"
                            SELECT label, node_type, file_path, line_start, description
                            FROM graph_nodes
                            WHERE project_id = ?1 AND is_god_node = 1 AND (deleted_at IS NULL OR deleted_at = '')
                            "#,
                        )?;
                        let rows = stmt.query_map(rusqlite::params![pid], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Option<String>>(2)?,
                                r.get::<_, Option<i64>>(3)?,
                                r.get::<_, Option<String>>(4)?,
                            ))
                        })?;
                        let mut lines = vec!["# Ключевые архитектурные хабы (God Nodes)\n".to_string()];
                        for r in rows.flatten() {
                            let (lbl, ntype, fpath, lstart, desc) = r;
                            let loc = if let (Some(f), Some(l)) = (fpath, lstart) {
                                format!("{}:{}", f, l)
                            } else {
                                "-".to_string()
                            };
                            lines.push(format!("- **`{lbl}`** (`{ntype}`) в `{loc}`: {}", desc.unwrap_or_default()));
                        }
                        Ok(lines.join("\n"))
                    }).unwrap_or_else(|e| format!("Ошибка: {e}"));
                    (text, "text/markdown")
                } else {
                    ("Активный проект не привязан к сессии.".to_string(), "text/markdown")
                }
            }
            "project://current/schema" => {
                if let Some(pid) = active_proj {
                    let text = self.ctx.db.with_conn(|conn| {
                        let mut stmt = conn.prepare(
                            r#"
                            SELECT label, file_path, description
                            FROM graph_nodes
                            WHERE project_id = ?1 AND node_type = 'Table' AND (deleted_at IS NULL OR deleted_at = '')
                            "#,
                        )?;
                        let rows = stmt.query_map(rusqlite::params![pid], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, Option<String>>(1)?,
                                r.get::<_, Option<String>>(2)?,
                            ))
                        })?;
                        let mut lines = vec!["# Схема данных и таблицы проекта\n".to_string()];
                        for r in rows.flatten() {
                            let (lbl, fpath, desc) = r;
                            lines.push(format!("### Таблица `{lbl}` (файл: `{}`)", fpath.unwrap_or_default()));
                            if let Some(d) = desc {
                                lines.push(d);
                            }
                            lines.push("".to_string());
                        }
                        Ok(lines.join("\n"))
                    }).unwrap_or_else(|e| format!("Ошибка: {e}"));
                    (text, "text/markdown")
                } else {
                    ("Активный проект не привязан к сессии.".to_string(), "text/markdown")
                }
            }
            "memory://context" => {
                match self.ctx.memory.build_context(20, None) {
                    Ok(ctx) => (ctx, "text/markdown"),
                    Err(e) => (format!("Ошибка получения контекста памяти: {e}"), "text/markdown"),
                }
            }
            _ => (format!("Ресурс не найден: {uri}"), "text/plain"),
        }
    }

    async fn get_prompt(&self, name: &str, args: &serde_json::Value, active_proj: Option<&str>) -> serde_json::Value {
        match name {
            "explain_component" => {
                let comp_name = args.get("component_name").and_then(|v| v.as_str()).unwrap_or("");
                let pid = active_proj.unwrap_or("");

                let mut context_lines = Vec::new();
                let _ = self.ctx.db.with_conn(|conn| {
                    let mut stmt = conn.prepare(
                        r#"
                        SELECT id, label, node_type, file_path, line_start, line_end, description, is_god_node
                        FROM graph_nodes
                        WHERE (project_id = ?1 OR ?1 = '')
                          AND (label = ?2 OR label LIKE ?3)
                          AND (deleted_at IS NULL OR deleted_at = '')
                        LIMIT 1
                        "#,
                    )?;
                    if let Ok(node) = stmt.query_row(rusqlite::params![pid, comp_name, format!("%{comp_name}%")], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<i64>>(4)?,
                            r.get::<_, Option<i64>>(5)?,
                            r.get::<_, Option<String>>(6)?,
                            r.get::<_, i64>(7).unwrap_or(0) == 1,
                        ))
                    }) {
                        let (nid, lbl, ntype, fpath, lstart, lend, desc, is_god) = node;
                        context_lines.push(format!("Компонент: `{lbl}` [{ntype}]"));
                        if let (Some(f), Some(ls), Some(le)) = (fpath, lstart, lend) {
                            context_lines.push(format!("Расположение: {}:{}-{}", f, ls, le));
                        }
                        if is_god {
                            context_lines.push("Статус: 👑 Ключевой хаб архитектуры (God Node)".to_string());
                        }
                        if let Some(d) = desc {
                            context_lines.push(format!("Описание: {d}"));
                        }

                        // Кто вызывает
                        let mut in_stmt = conn.prepare(
                            r#"
                            SELECT s.label, s.node_type, e.label
                            FROM graph_edges e
                            JOIN graph_nodes s ON s.id = e.source_id
                            WHERE e.target_id = ?1 AND (e.deleted_at IS NULL OR e.deleted_at = '')
                            LIMIT 10
                            "#,
                        )?;
                        let in_rows = in_stmt.query_map(rusqlite::params![nid], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                        })?;
                        let mut callers = Vec::new();
                        for r in in_rows.flatten() {
                            callers.push(format!("`{}` [{}] через {}", r.0, r.1, r.2));
                        }
                        if !callers.is_empty() {
                            context_lines.push(format!("Вызывается из: {}", callers.join(", ")));
                        }

                        // Кого вызывает
                        let mut out_stmt = conn.prepare(
                            r#"
                            SELECT t.label, t.node_type, e.label
                            FROM graph_edges e
                            JOIN graph_nodes t ON t.id = e.target_id
                            WHERE e.source_id = ?1 AND (e.deleted_at IS NULL OR e.deleted_at = '')
                            LIMIT 10
                            "#,
                        )?;
                        let out_rows = out_stmt.query_map(rusqlite::params![nid], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                        })?;
                        let mut callees = Vec::new();
                        for r in out_rows.flatten() {
                            callees.push(format!("`{}` [{}] через {}", r.0, r.1, r.2));
                        }
                        if !callees.is_empty() {
                            context_lines.push(format!("Зависит от: {}", callees.join(", ")));
                        }
                    }
                    Ok(())
                });

                let ctx_str = if context_lines.is_empty() {
                    format!("Компонент `{comp_name}` в графе проекта не найден.")
                } else {
                    context_lines.join("\n")
                };

                serde_json::json!({
                    "description": format!("Контекстное объяснение компонента {comp_name}"),
                    "messages": [
                        {
                            "role": "user",
                            "content": {
                                "type": "text",
                                "text": format!(
                                    "Изучи контекст компонента из графа проекта:\n\n{}\n\nОбъясни архитектурную роль компонента `{comp_name}`, его ответственность и возможные риски изменений.",
                                    ctx_str
                                )
                            }
                        }
                    ]
                })
            }
            "plan_feature" => {
                let task = args.get("task_description").and_then(|v| v.as_str()).unwrap_or("");
                let pid = active_proj.unwrap_or("");

                let overview = if !pid.is_empty() {
                    self.ctx.db.with_conn(|conn| crate::graph::GraphAnalytics::generate_project_report(conn, pid))
                        .map(|r| r.markdown_summary)
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                serde_json::json!({
                    "description": "Архитектурное планирование новой фичи на базе графа проекта",
                    "messages": [
                        {
                            "role": "user",
                            "content": {
                                "type": "text",
                                "text": format!(
                                    "Задача на реализацию:\n{}\n\nАрхитектурный контекст проекта:\n{}\n\nСоставь подробный план реализации фичи: определи затронутые модули, ключевые хабы связности и шаги тестирования без риска регрессии.",
                                    task,
                                    if overview.is_empty() { "Проект не выбран." } else { &overview }
                                )
                            }
                        }
                    ]
                })
            }
            _ => serde_json::json!({
                "description": "Неизвестный шаблон промпта",
                "messages": []
            }),
        }
    }
}
