# Карта портирования из omnes-aibot

Источник: `C:\Projects\omnes-aibot\OmnesBOT\backend\app` (READ-ONLY).
Порядок использования: перед каждой задачей `PLAN.md` найти здесь исходник,
прочитать его, упростить и перенести. Правила упрощения — `AGENTS.md` §3, §6.

> ⚠️ В omnes-aibot есть схемный дрейф (запросы к несуществующим колонкам/таблицам:
> `graph_entities`, `edge_type` в `graph_edges`, колонки `scope/scope_id` у GBAM).
> Они молока падают в `except`-ветки. Портировать **только перечисленные ниже файлы
> и только описанные механики**.

---

## Что переносим (по фазам плана)

### Фаза 2 — память и workspace

| Целевой модуль | Источник в omnes-aibot | Что брать |
|---|---|---|
| `memory_service.py` | `app/services/memory_service.py` — `MemoryService` | Схема `agent_memories`; `search_fts` (у нас FTS5 вместо tsvector), `search_vector` (у нас numpy вместо pgvector), **`search_hybrid` с RRF k=60 — перенести формулу как есть**; скоринг `build_context` `0.6*importance + 0.4*word-overlap`; `decay_importance`, purge `importance<0.05 AND access_count<2` |
| `workspace.py` | `app/core/omnesbot/agent/memory.py` — `MemoryStore` | Файлы `MEMORY.md`/`SOUL.md`/`USER.md`; `history.jsonl` со схемой `{"cursor","timestamp","content"}`; dot-курсоры; `compact_history()` (макс. 1000); идея `[RAW]`-архива как деградированного режима без LLM |
| `gitstore.py` | `app/core/omnesbot/utils/gitstore.py` — `GitStore` | auto_commit избранных MD-файлов, перечисление истории, восстановление файла из коммита |

### Фаза 3 — консолидация

| Целевой модуль | Источник | Что брать |
|---|---|---|
| `consolidator.py` | `app/core/omnesbot/agent/memory.py` — `Consolidator` | Триггер по оценке токенов бюджета `(context_window − max_completion − 1024)/2`; границы по user-ходам; лимиты 60 сообщений / 5 раундов; аппенд результатов в `history.jsonl` и продвижение `session.last_consolidated` (у нас — `.cursor`) |
| шаблон суммаризации | `app/core/omnesbot/templates/agent/consolidator_archive.md` | Смысл промпта (сжать диалог в итоговые факты), переписать под локальный контекст |
| схема daily-логов | `app/services/omnesbot/memory_v2.py` — `MemoryV2Manager` | Только схема события `daily/YYYY-MM-DD.jsonl` (timestamp/query/answer_preview/source) — как вход для автодрима и ретеншна. Топики/индексы/scope-изоляцию — не переносить |

### Фаза 4 — граф знаний

| Целевой модуль | Источник | Что брать |
|---|---|---|
| `extractor.py` | `app/services/oneke/extractor.py` — `OneKEExtractor` | `_split_into_chunks`: границы предложений, `CHUNK_MAX_CHARS=3000`, перекрытие 300, префильтр (<80 симв. / только заголовок); скользящее суммаризационное окно при >100 чанков; LLM-промпт извлечения (компактный JSON: entities `{id,label,type,description}`, relations `{source,target,label,contexts}`); семафор=2; инкрементальные сейвы каждые 20 чанков; ретраи с backoff |
| пост-обработка | `app/api/v1/extraction.py` | `_validate_relation_targets`; `_infer_relations_from_descriptions` — перенести ~40 базовых русских шаблонов (не все ~120); `_filter_junk_entities` (стоп-слова); `_save_entities_to_db` — механика дедупа по label с инкрементом `val`/`weight` и склейкой описаний; `_generate_embeddings` (`"{label}: {description}"`) |
| `graph_service.py` (поиск) | `app/services/kag_reasoning.py` — `KAGReasoningService` | **Только PG-путь** (`_search_graph_pg`): ILIKE + скоринг label=10/name=5/desc=1, сбор рёбер между найденными узлами; `_search_vector`; LLM-rerank фолбэк `_search_documents_llm`; `reason()`: сборка блока фактов → один LLM-вызов → JSON `{answer, confidence, reasoning_steps, used_entities, used_relations}` |
| `ingest.py` | `app/api/v1/extraction.py` (парсинг) | pypdf для PDF, python-docx для DOCX, детект кодировки; фоновые очереди задач — не переносить, делаем синхронно с лимитом чанков |

### Фаза 5 — дриминг

| Целевой модуль | Источник | Что брать |
|---|---|---|
| `dream.py` | `app/core/omnesbot/agent/memory.py` — `Dream` | Батч 20 записей от `.dream_cursor`; **фаза 1** — LLM-анализ (шаблон `templates/agent/dream_phase1.md`: новая история + текущие MEMORY/SOUL/USER); **фаза 2** — `AgentRunner.run()` агентный цикл ≤10 итераций с ReadFile/EditFile и шаблоном `dream_phase2.md` (у нас — свои `_dream_read`/`_dream_edit`); продвижение курсора; compact; auto_commit |
| шаблоны дрима | `app/core/omnesbot/templates/agent/dream_phase1.md`, `dream_phase2.md` | Структуру промптов перенести, адаптировав имена файлов/инструментов |
| `autodream.py` | `app/services/omnesbot/autodream.py` — `AutoDreamWorker` | Гейты: ≥4ч с прошлого запуска, ≥10 новых daily-событий, lock-файл (stale 1ч); состояние `autodream_last_run.json`; интервал проверки 5 мин. Вместо `memory_v2.run_maintenance()` — вызов `dream.py` + ретеншн |
| dream-команды | `app/core/omnesbot/command/builtin.py` (`cmd_dream`, dream-log/restore) | UX-семантика ручного запуска и отката |

### Фаза 6 — бэкап/ретеншн

| Целевой модуль | Источник | Что брать |
|---|---|---|
| `backup.py` | — (новое) | `VACUUM INTO` + копия workspace + ротация 14 |
| ретеншн | `memory_v2.run_maintenance()` | Идея prune по `RETENTION_DAYS` и дедуп — упростить до удаления старых daily-файлов |

## Что НЕ переносим (осознанно)

| Компонент omnes-aibot | Причина |
|---|---|
| `app/services/kag/**`, `app/services/knext/**` (vendored OpenSPG) | Мёртвый код в самом OmnesBOT; KAGReasoner там — shim к собственному сервису |
| `app/services/openspg/**` (Micro-OpenSPG) | Фасад-заглушка, `schema_query` возвращает пустую схему |
| `app/services/dream_distill.py` | Dead code, ничего не импортирует |
| `app/services/omnesbot/gbam.py` (GBAM) | Тяжело, зависит от Neo4j/scope-колонок; идея зеркала сессий — в бэклог |
| `app/services/omnesbot/memory_layer3.py` | Хорошая идея (профиль пользователя) — в бэклог |
| `app/services/agent_memory.py` (Neo4j AgentMemory) | Neo4j исключён (ADR-6) |
| Neo4j-ветки везде (`app/db/neo4j.py`) | ADR-6: SQLite-граф |
| Мультиарендность: companies/users/groups/RBAC, `knowledge_graphs` с scope/graph_type, governance (`ToolGovernanceSpec`) | Один локальный пользователь (ADR из AGENTS.md §3) |
| Telegram/channels/frontend, API-слои v1 | Не нужен: доступ только через MCP |
| spaCy NER-подсказки экстрактору, sentence-transformers дедуп чанков | Упрощение: дедуп по эмбеддингам нашего провайдера; spaCy не тянем |

## Проверенные факты об окружении omnes-aibot, влияющие на перенос

- Формула RRF: `score = Σ 1/(60 + rank)` — перенести без изменений.
- Эмбеддинги OmniBOT: MiniLM 384d по умолчанию — наш дефолт тоже 384d
  (`multilingual-e5-small`), размерность хранится в `kv.embed_dim`.
- Промпт извлечения в OneKE уже даёт JSON на русском — переиспользовать стиль,
  но явно потребовать строгий JSON без markdown-обёрток.
