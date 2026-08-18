# Walkthrough: Полный перенос OB2H на Rust (`ob2h_rust`)

Проект [`ob2h`](file:///c:/Projects/omnesbot_for_hermes/ob2h) полностью портирован на **Rust (1.97+)** в директории [`c:\Projects\omnesbot_for_hermes\ob2h_rust`](file:///c:/Projects/omnesbot_for_hermes/ob2h_rust).

---

## 📦 Реализованные модули

1. **Конфигурация (`src/config.rs`)**:
   - Полная поддержка всех переменных окружения `OB2H_*` (`DATA_DIR`, `LLM_*`, `EMBED_*`, `AUTODREAM_*`, `RETENTION_DAYS`, `LOG_LEVEL`).
2. **База данных и FTS5 (`src/db/`)**:
   - SQLite с версионными миграциями, WAL режимом, `foreign_keys=ON`.
   - Таблицы `kv`, `memories`, `memory_relations`, `documents`, `chunks`, `graph_nodes`, `graph_edges`, `dream_runs`.
   - Полнотекстовый поиск FTS5 с русским токенизатором `trigram` и автоматическими триггерами синхронизации (INSERT/UPDATE/DELETE).
3. **Векторная математика (`src/vector/`)**:
   - Сериализация и десериализация `f32` BLOB (little-endian).
   - Косинусное сходство, L2-нормализация, `top_k` перебор.
   - Reciprocal Rank Fusion (RRF $k=60$) для гибридного ранжирования.
4. **Эмбеддинги (`src/embedding/`)**:
   - Трейт `EmbeddingProvider`, `ApiEmbedding` (OpenAI-совместимый `/v1/embeddings`), `FakeEmbedding` (детерминированный MD5 хэш для тестов).
5. **Память (`src/memory/`)**:
   - `MemoryService`: `save`, `get`, `update`, `forget`, `search_fts`, `search_vector`, `search_hybrid`, `decay_importance`, `purge_weak`, `build_context` (`<agent_memory>`).
6. **Воркспейс и Git (`src/workspace/`)**:
   - `Workspace`: `SOUL.md`, `USER.md`, `MEMORY.md`, `history.jsonl`, `daily/*.jsonl`, компактификация и курсоры.
   - `GitStore`: авто-коммиты изменений воркспейса, просмотр истории, откат версий.
7. **LLM и консолидация (`src/llm/`, `src/consolidator/`)**:
   - `LLMClient` & `LLMClientExt` (`ask`, `ask_json`) с поддержкой retry/backoff.
   - `Consolidator`: суммаризация сессий по бюджету токенов и продвижение курсора.
8. **Экстракция и Граф KAG-lite (`src/extractor/`, `src/graph/`)**:
   - OneKE-пайплайн: разделение на предложения, чанкинг с перекрытием, префильтрация, инференс отношений по 40 русским шаблонам, фильтрация стоп-слов, дедупликация нод по SHA256.
   - `GraphService`: 1-hop обход, поиск по графу, KAG-рассуждение с синтезом ответа и оценкой уверенности.
9. **Дриминг и автодрим (`src/dream/`)**:
   - 2-фазный дриминг (анализ + агентный цикл точечных правок MD-файлов + Dream Extract сессий в граф).
   - `AutoDreamWorker`: фоновый Tokio-worker с гейтами ($\ge 4$ч, $\ge 10$ событий, файловый lock, decay/purge).
10. **Бэкапы (`src/backup/`)**:
    - Атомарный снимок через `VACUUM INTO` + снапшот воркспейса, ротация 14 копий.
11. **MCP Server & CLI (`src/mcp/`, `src/cli/`, `src/main.rs`)**:
    - Stdio JSON-RPC 2.0 сервер со всеми **18 MCP-инструментами**.
    - CLI интерфейс на базе `clap`: `serve`, `dream run/status/log/restore`, `backup`, `stats`.

---

## 🧪 Результаты тестирования и сборки

### 1. Unit и Integration тесты (`cargo test`)
Все **15 тестов** завершились успешно:
- `vector::similarity::tests::test_cosine` — **OK**
- `vector::rrf::tests::test_rrf_merge` — **OK**
- `vector::similarity::tests::test_serialize_deserialize` — **OK**
- `embedding::fake::tests::test_fake_embedding_deterministic` — **OK**
- `db::tests::test_in_memory_db_migrations` — **OK**
- `test_database_initialization_and_fts_trigram` — **OK**
- `test_dream_2_phase_cycle` — **OK**
- `test_extractor_with_fake_llm` — **OK**
- `test_sentence_splitting_and_chunking` — **OK**
- `test_graph_upsert_search_and_reason` — **OK**
- `test_mcp_all_tools_dispatch` — **OK**
- `test_memory_crud_and_hybrid_search` — **OK**
- `test_vector_serialization_and_similarity` — **OK**
- `test_top_k_and_rrf_merge` — **OK**
- `test_workspace_files_and_history` & `test_git_store_auto_commit` — **OK**

### 2. Сборка релиза (`cargo build --release`)
- Создан автономный исполняемый файл: [`c:\Projects\omnesbot_for_hermes\ob2h_rust\target\release\ob2h.exe`](file:///c:/Projects/omnesbot_for_hermes/ob2h_rust/target/release/ob2h.exe).
- Проверена работа CLI:
  - `ob2h.exe --help`
  - `ob2h.exe stats` -> `memories=0 relations=0 documents=0 chunks=0 graph_nodes=0 graph_edges=0 dream_runs=0 db=4KB`
  - `ob2h.exe dream status` -> `last_run: никогда, dream_cursor: none`
