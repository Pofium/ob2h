# Changelog

Формат: Keep a Changelog (упрощённый). Версии — по мере появления пользовательского
контракта (MCP-инструментов).

## [Не выпущено]

### Added
- **Фоновый AST-скан по MCP (MCP-only scan, запись в PLAN_v1.4 §6)**: `project_scan`
  больше не упирается в 60-с таймаут MCP-клиента — скан уходит в фоновый job
  (`src/project/scanjob.rs`): AST-скан → пересчёт God Nodes → векторизация узлов
  (хвост добивается циклами по 500, потолок 200 итераций). По умолчанию инструмент
  ждёт до 45 с (малые проекты успевают целиком), долгий скан отвечает `still running`
  и досматривается новым инструментом **`project_scan_status`** (job-статус + сводка
  из БД: ast_nodes/edges/god_nodes/pending_embedding/last_scanned). `wait=false` —
  мгновенный ack запуска. Повторный `project_scan` по идущему скану того же проекта
  идемпотентен (возвращает текущий job). Агенту больше не нужно уходить в CLI/логи/БД,
  чтобы инициировать или проверить построение AST-базы. Контракт: 35 → 36 инструментов,
  у `project_scan` новый опциональный аргумент `wait`. +тесты в `tests/test_mcp.rs`
  (статус-поллинг, wait=false, [Error] по несуществующему id). В ветку feat/v1.4
  вмержена линия релиза v1.4.0 (trust-флаг prefetch, актуализация доков), не
  попадавшая в ветку.

### Changed
- CLI `project scan` остаётся без изменений (ручные/скриптовые запуски), но больше
  не единственный способ досмотреть долгий скан из агента.

## [1.4.0] — 2026-09-13

### Added
- **Trust в скоринге prefetch (аудит v1.3, §22.2) — за флагом, дефолт off**:
  `ContextOptions.trust_weight` + `OB2H_CONTEXT_TRUST_WEIGHT` (дефолт `0` — прежняя
  формула; `0.2` — слагаемое `trust` из плана v1.3). Механизм реализован и протестирован
  (`trust_boost_lifts_record_in_prefetch_scoring`, `default_trust_weight_keeps_relevance_order`),
  но выключен по bench-данным живой БД: на молодой trust-статистике (миграция M5 сегодня,
  ревизия дрима уже ставила −0.4/−0.5) включение веса 0.2 дало recall@5 0.569→0.417 (−27%),
  MRR 0.611→0.444 — за красной линией гейта ADR-13 (−10%/−15%). Решение о включении —
  после накопления ночных прогонов (`ob2h bench --mode history`), цифры — в
  `docs/bench_baseline.md`.
- **Ночная статистика bench (Ф35.2/30.3, доделано)**: каждый bench-прогон дописывает
  `data/bench/history.jsonl` (§4: ts/mode/recall/mrr/p95/db_size/embedding_backend/
  vec0/dream_sha); сервер раз в 24 ч сам гонит latency-бенч по golden-набору
  (`OB2H_BENCH_NIGHTLY=0` — off); `ob2h bench --mode history` — сводка с медианами
  p95 off/on и честным вердиктом 35.2. +2 теста (`tests/test_bench_history.rs`).
- **Communities, framework edges, связка с памятью (Фаза 40 / трек C PLAN_v1.4)**:
  зоны (label propagation + модулярность Q, детерминированный pure-Rust) в
  `project_report`; framework-эвристики ROUTE (axum/actix/FastAPI) и QUERIES_TABLE
  (SQLAlchemy `__tablename__`) с provenance=INFERRED; `memory_save` с project_id
  линкует упомянутые God Nodes/символы в meta.code_symbols (OneKE-lite);
  `project_context(mode=repo_map, with_memory=true)` / CLI `--memory` подмешивают
  high-trust память проекта; dead-code исключает маршруты (ROUTE/HANDLES).
  Модуль `src/graph/communities.rs`; +4 теста (`tests/test_track_c5.rs`).
- **Type-resolve lite + provenance (Фаза 39 / трек C PLAN_v1.4)**: лёгкий semantic
  pass Rust+Python на своём AST — резолв простых вызовов (same-file, импорты,
  алиасы `use x as y`/`from .m import x as y`) в CALLS-рёбра с provenance=RESOLVED;
  нерезолвленное остаётся без ребра (AMBIGUOUS). M8: provenance на graph_edges
  (EXTRACTED/RESOLVED/INFERRED; легаси-рёбра читаются как есть). Provenance в
  выдаче `project_call_path` и `project_impact`. +4 теста (`tests/test_typepass.rs`).
- **Edit-time blast radius, warn-only (Фаза 38 / трек C PLAN_v1.4)**: MCP-ресурс
  `project://current/blast-radius` + plugin-мост в prefetch — короткий warn-блок
  «правка `<symbol>`: затронуты <callers>» по последним изменённым символам
  (TTL 30 мин, kv-кэш `blast_hint:<project>`); флаг `OB2H_EDIT_BLAST=warn`
  (дефолт off), fail-open. Модуль `src/graph/blast.rs`; +3 теста (`tests/test_blast.rs`).
- **Repo-map под token budget (Фаза 37 / трек C PLAN_v1.4)**: `project_context
  mode=repo_map` — карта «файл → сигнатуры символов», уложенная в бюджет токенов
  (2k/4k/8k): PPR-ранжирование по file-dependency графу (прямые file→file IMPORTS +
  ко-импортная связность 1/|файлы модуля|), бинарный поиск максимального префикса
  под бюджет, query-фокус через PPR-сиды. CLI `ob2h project repo-map`. Дефолтный
  `project_context` без mode — прежний формат (совместимость).
  Модуль `src/graph/repomap.rs`; +4 теста (`tests/test_repomap.rs`).
- **Structural queries по кодовому графу (Фаза 36 / трек C PLAN_v1.4)**: новый
  MCP-инструмент **`project_call_path` (№35)** — явная цепочка вызовов/зависимостей
  `from→to` (BFS по `CALLS/IMPORTS/IMPLEMENTS/DEPENDS_ON`) либо callers/callees
  с глубиной; режимы `mode=callers|callees` в `project_graph_search`; CLI
  `ob2h project dead-code` и секция «Кандидаты в мёртвый код» в `project_report`
  (in-degree 0, исключая entrypoints: main, тесты, pub API; расширяется в Ф40).
  Модуль `src/graph/callpath.rs`; +5 тестов (`tests/test_callpath.rs`).
- **Латентность и гигиена (Фаза 35 PLAN_v1.4)**: bench-режим `--mode latency` —
  p50/p95 отдельно по `memories`, `graph_nodes` и эмбеддингу запроса (пол);
  автосохранение latency-секции в `docs/bench_baseline.md` (`--save-baseline`,
  recall-часть не затирается). **Эксперимент sqlite-vec** (35.1, флаг `OB2H_VEC0`):
  vec0-индекс над `graph_nodes` (`src/vector/vec0.rs`, `sqlite-vec 0.1.9`, режимы
  `int8`/`bit` через `OB2H_VEC0_MODE`, oversample `OB2H_VEC0_OS`), первый проход
  + рескоринг нашими векторами, CLI `ob2h vec0 build|stats|recall`. На копии живой
  БД: **recall@10 = 1.0000** при os=2 (критерий ≥ 0.99 выполнен), векторный скан
  566 → 192 мс (**2.9×**), end-to-end p95 2408 → 2093 мс (доминирует лексический
  полный скан, а не вектор) → флаг оставлен opt-in, разбор в
  `docs/ADR-35.1-sqlite-vec.md`. MCP tool annotations (35.3): `readOnlyHint` на 15
  read-only инструментов, `destructiveHint` на `memory_forget`/`dream_restore` —
  видны в `tools/list`, контракт аргументов не тронут. Ретеншн workspace-логов (35.4):
  daily-логи старше `OB2H_LOG_RETENTION_DAYS=90` упаковываются в
  `data/workspace/archive/YYYY-MM.jsonl.gz` при старте (архивация, не удаление;
  свежие и «чужие» файлы не трогаются). +5 тестов (`tests/test_f35.rs`) и
  +5 тестов vec0 (`tests/test_vec0.rs`, синтетика 10K: recall@10 int8 = 1.0000,
  bit = 0.30–0.54 — зафиксировано честно).
- **Sync v2 (Фаза 34 PLAN_v1.4)**: дельта-экспорт по курсору пира (`sync_state.last_export_at`)
  + `ob2h sync push --full` — полный бандл; заголовок несёт `version: 2`. В v2-бандл входят
  `memory_links` (с M7 soft-delete), `trust`, `last_feedback_at` — v1 их не возил; `ralph_*`/
  `ast_changes` по-прежнему вне (ADR-K6). **Поле-уровневый merge при apply**: `meta` —
  глубокое объединение (входящие ключи выигрывают), `access_count` — max; конфликт `content`
  всегда пишется в `sync/conflicts.jsonl` (журнал обоих направлений, дополняет счётчик 25.4),
  при `OB2H_SYNC_KEEP_LOSERS=1` проигравшая версия сохраняется в `meta.conflict_versions`
  (StateFuse, §8). v1-бандлы читаются по старой семантике. **`ob2h sync verify [--peer]`** —
  сверка без переноса: counts, trust_avg, контрольные суммы по `key`+`updated_at` (записи) и
  `from+to+kind+created_at+deleted_at` (рёбра), отчёт о дрейфе (удалённая сторона — по ssh,
  новые поля пира `data_dir`/`bin`). +6 тестов (`tests/test_sync_v2.rs`).
- **Typed edges в dream-ревизии (Фаза 32 PLAN_v1.4, миграция M7 — схема 6→7)**:
  вердикты ревизора становятся рёбрами — `contradicted`+related_key → `contradicts`,
  `outdated`+related_key → `supersedes` (направление от новой записи к старой, upsert
  без дублей на повторных дримах); `confirmed` — как раньше, только trust-bump.
  **Conflict-разметка**: в `memory_search` блок `[conflicts]` показывает спор целиком —
  обе стороны с trust и датой вердикта. **Belief-derivation lite**: LLM предлагает пары
  `causes` (флаг `OB2H_DREAM_BELIEF`, дефолт off — предложения только в дрим-отчёте).
  **Миграция V7**: `memory_links.deleted_at` — forget/merge рвут связи soft-delete'ом
  (tombstone реплицируется синком v2 в Ф34), чтения фильтруют удалённые, upsert
  оживляет. +7 тестов (`tests/test_typed_edges.rs`).
- **Ralph Knowledge Layer — ядро и окружение (Фазы 26–28 PLAN_v1.3, миграция M6 → схема v6)**:
  таблицы `ralph_runs`/`ralph_iterations`/`ralph_findings`/`ast_changes` (аддитивно,
  graph_nodes не пересоздавался — confidence сохранена, тест схемы). MCP-инструменты
  **№27–33**: `ralph_start`, `ralph_iteration` (авто-вердикт только по `tests_summary`
  — ADR-K4; идемпотентность по (run, task, n); AST-рескан с symbol-level дельтой;
  findings-маркеры Ponytail), `ralph_verdict`, `ralph_context` (спека → негативный
  опыт → reuse-кандидаты из AST → инварианты → god nodes; lite/full; выгрузка на диск
  + context_ref), `ralph_report` (debt-леджер, gain: reuse-hit rate), `ast_diff`,
  `ast_history`. Staleness-pass: изменённые символы → findings `stale` (повторно не
  перемечаются). CLI `ob2h ralph findings-to-memory --project <id> [--dry-run]` (FR-K12);
  doctor — сироты-раны; `omnes_stats` — блок ralph. Dream-фаза 3 (FR-K7): ревизия
  stale-findings LLM по свежей истории (reverified → verified source=dream; флаг
  `OB2H_DREAM_RALPH`, дефолт on; сводка в dream_runs.stats и коммит-сообщении).
  Скилл `skills/ralph-loop/SKILL.md` (лестница минимальности, red tests ≠ готово).
  `ralph_*`/`ast_changes` вне синк-бандлов (ADR-K6) — см. SYNC.md.
- **Надёжность и мультиагентность (Фаза 25 PLAN_v1.3)**: громкий FakeEmbedding —
  runtime-бэкенд в `omnes_stats` (`backend=fake|local_bert|api`), `[warn]` в выдаче
  `memory_search` при деградации, `ob2h doctor` — реальная канареечная проверка
  (загрузка модели + embed, красный статус при fallback). Мультиагентность:
  плагин `sync_turn(turn_author=..., **kwargs)` — автор хода в `meta.author`
  daily-записей; `memory_context`/`session_log`/`session_ingest` + опциональный
  `author` (записи с чужим meta.author исключаются из prefetch-блока). `sync status` —
  накопленный счётчик проигранных LWW-конфликтов. 25.3 (реранкер) отложена —
  ort-зависимость требует отдельного решения (ADR-14 §8).
- **PPR по памяти / движок Personalized PageRank (Фаза 33 PLAN_v1.4 + 29.1 v1.3)**:
  новый `src/graph/pagerank.rs` — generic итеративный PPR без новых зависимостей
  (damping клампится 0.5–0.85, ≤20 итераций, сходимость по L1, degree-normalization
  для hub-защиты, масса висячих узлов возвращается в персонализацию, веса по типу ребра
  `OB2H_PPR_WEIGHTS`, `normalize_entity` для схлопывания форм сущностей).
  Память как граф: `MemoryService::ppr_rank` (узлы — живые записи, рёбра — `memory_links`
  с весом kind × вес ребра, dual-seed: гибридные хиты + entity-фразы),
  `ppr_expand_records` и `ppr_context`. MCP: `memory_search mode=graph` (блок `[ppr]`
  вместо 1-hop, фолбэк на 1-hop), `graph_search mode=ppr` (PPR по графу знаний: dual-seed
  из матчинга, веса рёбер по типу через `OB2H_PPR_WEIGHTS`, проектный фильтр) и
  `graph_reason scope=memory|all` (PPR-подграф памяти,
  уверенность по trust × PPR-массе, лимиты 500 узлов / 1 с; без `scope` — прежний ответ
  по графу знаний). Новые настройки: `OB2H_PPR_WEIGHTS`, `OB2H_PPR_DAMPING`,
  `OB2H_GRAPH_REASON_MEMORY_MAX_NODES`, `OB2H_GRAPH_REASON_MEMORY_TIMEOUT_MS`.
  +9 юнит-тестов движка и 5 интеграционных (`tests/test_pagerank.rs`).
- **Save-time дедуп + `memory_merge` (Фаза 31 PLAN_v1.4, завершение — 31.1/31.4)**:
  `memory_save` без LLM находит топ-1 косинусного соседа — cos ≥ 0.98: identity-дубль,
  тихий UPDATE существующей записи (union meta, max importance, +1 access_count, свежая
  формулировка), новой строки нет; cos 0.75–0.98: подозрение → маркер
  `meta.merge_candidate` на новой записи (вердикт — офлайн в дриме). MCP-инструмент
  **`memory_merge` (№34)** `memory_merge(keys[], canonical_key?, note?)` — явное
  подтверждённое слияние (каноническая = максимум trust/importance или явно заданная):
  union meta, max importance, sum access, tombstone поглощённых с `meta.merged_into`,
  редирект memory_links. Единый движок `MemoryService::merge_records` используется и
  дрим-вердиктом merge (31.2). +3 теста на ScriptedEmbedding.
- **Офлайн-консолидация в дриме (Фаза 31 PLAN_v1.4, частично — 31.2/31.3/31.5)**:
  модуль `src/dream/consolidate.rs` — LLM-вердикты по группам `meta.merge_candidate`
  (`merge | keep_both | contradicts | supersedes`, исходы MELD; ≤5 групп за дрим):
  merge — union meta / max importance / sum access на канонической записи, tombstone
  поглощённой с `meta.merged_into` и редиректом memory_links; supersedes/contradicts —
  обе записи живы + typed edge (резерв 23.5 начинает работать, повторный дрим не
  дублирует рёбра). Отчёт — в `dream_status` (поле `consolidation` в stats дрима).
  Compaction раз в 30 дней (kv `compaction:last`): кластеры слабых записей
  (Jaccard ключей + косинус, ≤8, без high-trust/high-access) → summary-узел
  `hmem-digest/<дата>`, kind=summary на членов; оригиналы не трогаются. CLI
  `ob2h memory dedup [--dry-run]`: топ-1 косинусный сосед (0.98 identity / 0.75
  подозрение), без --dry-run ставит маркеры `merge_candidate` — слияние решает дрим
  или `memory_merge` (31.4, файлы mcp/* ждут коммита WIP Ф25). +6 тестов
  (test_consolidation.rs).
- **Ночной bench-гейт дрима (Фаза 30 PLAN_v1.4)**: после успешного автодрима прогон
  quick-набора golden set (15 кейсов, mode=context, бюджет `OB2H_BENCH_GATE_TIMEOUT_MS=3000`);
  относительное падение recall@5 >10% или MRR >15% против kv `bench:last` →
  `dream_restore` на workspace-коммит до дрима + алерт «ОТКАТ: …» в дрим-отчёт
  (dual-сигнал: падение только MRR — тоже откат). Timeout ≠ rollback: warning,
  `bench:last` не трогается. Workspace-инвариант: обвал tracked-файла (>50% строк
  относительно коммита до дрима) — тоже rollback (bench по БД порчу MD-файлов не видит).
  Гейт выключен по умолчанию (`OB2H_BENCH_GATE=1` — включить), без golden set — skip
  с warning. История прогонов: `data/bench/history.jsonl` (ts, gate, recall@5/10, MRR,
  p95, db_size_mb, embedding_backend, dream_sha); CLI `ob2h bench history [--last N]`;
  исход последнего гейта — в `dream_status` (поле `bench_gate` в stats). Счётчик
  `bench:runs` — для решения по реранкеру в Ф35.2.
- **Лёгкая база и бэкапы (Фаза 24 PLAN_v1.3)**: int8-квантование эмбеддингов v2
  (`[0x01][scale f32][i8 × dim]`, ~4× компактнее f32; dual-read по magic-байту, все
  писатели пишут v2); CLI `ob2h db quantize-embeddings [--dry-run]` (пре-бэкап + VACUUM;
  на копии живой БД: 963 → 619 МБ, recall без деградации); `ob2h backup --scope quick`
  (память+связи+kv+воркспейс, ~десятки МБ) и `ob2h backup verify <path>`
  (integrity_check + сверка счётчиков с живой БД); раздельная ротация
  `OB2H_BACKUP_KEEP_FULL=3` / `OB2H_BACKUP_KEEP_QUICK=14`.
- **Trust и feedback-петля (Фаза 23 PLAN_v1.3, миграция M5 → схема v6)**: колонки
  `memories.trust` (дефолт 0.5) и `last_feedback_at`; таблица `memory_links`
  (kind: same_project|entity|category|manual; contradicts|causes|supersedes — резерв).
  touch/search/prefetch подтверждает записи (trust +0.02, кламп 1.0); decay гасит trust
  тем же фактором; при trust < 0.15 запись помечается `meta.candidate_for_forget=1`
  (никаких автоудалений). Dream-ревизия: 10 записей с минимальным trust → LLM-вердикты
  confirmed +0.1 / outdated −0.4 / contradicted −0.5 (флаг `OB2H_DREAM_MEMORY_REVISION`,
  дефолт on; дедуп-правило builtin в промпте дрима). Автосвязи при `memory_save`
  (до 5 свежих same_project + category), каскадное удаление связей при forget.
- **MCP-инструмент `memory_feedback` (№26)**: `memory_feedback(key, verdict:
  helpful|unhelpful|outdated, note?)` → trust +0.15/−0.2/−0.3, лог в `meta.feedback`.
- **`memory_search` + параметр `related`**: 1-hop соседи по memory_links отдельным
  блоком `[related]` (аддитивно, дефолт off).
- **CLI `ob2h bench`** (Фаза 21 PLAN_v1.3): регрессионный контур retrieval — golden set
  (`data/bench/golden.jsonl`, 36 кейсов), метрики recall@k / MRR / доля пустых / p50-p95
  латентности, режимы `search` (гибридный memory_search) и `context` (build_context),
  вывод таблица/JSON/markdown, `--save-baseline`. Baseline живой БД: search recall@5 0.222,
  MRR 0.198, p50 70 мс; context recall 0.833, MRR 0.861 — см. `docs/bench_baseline.md`.
- **Security**: `workspace_read`/`workspace_write` изолированы внутри workspace
  (запрет абсолютных путей и `..` — устранён path traversal), +6 тестов.

### Changed
- **`memory_context` / `build_context` — честный prefetch (Фаза 22 PLAN_v1.3)**:
  при непустом `query` блок `<agent_memory>` собирается гибридным пулом
  (FTS5-trigram + cosine, RRF k=60) со скорингом
  `0.35*rel + 0.25*importance + 0.1*trust(конст.) + 0.1*recency + 0.1*log1p(access)`,
  MMR-диверсификацией (λ=0.7) и бюджетом символов (обрезка по record-границам в Rust,
  маркер `…[truncated N records]`); пустой/тривиальный запрос — прежний importance-fallback.
  **Контракт**: `memory_context` получил опциональный `max_chars` (по умолчанию —
  `OB2H_PREFETCH_MAX_CHARS=8000`; `max_tokens` сохранён); touch_access инкрементирует
  только записи, вошедшие в блок. Конфиги: `OB2H_PREFETCH_MAX_CHARS`, `OB2H_HALF_LIFE_DAYS=90`.
  Замеры до/после — в `docs/bench_baseline.md`.

## [1.2.0] — 2026-09-03

### Added
- **Zero-Config Автодетект и Сессионный Контекст (Фаза 16)**:
  - Автоматическое определение корня проекта вверх по директориям (`find_project_root`) по маркерам `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `composer.json` и др.
  - Автоматическая регистрация проекта при MCP-инициализации (`initialize`) на базе `rootUri` / `workspaceFolders` / `current_dir()`.
  - Привязка активного проекта к сессии (`active_project_id` и `active_workspace`) — во всех инструментах параметр `project_id`/`id` стал опциональным.
- **Честный инкрементальный AST-скан и поддержка `.gitignore` (Фаза 17)**:
  - Таблица `project_files` для отслеживания SHA-256 хэшей и mtime файлов: повторный скан 1 000+ файлов занимает <30 мс.
  - Честная обработка удалённых файлов (пометка нод и рёбер как `deleted_at`).
  - Интеграция библиотеки `ignore` (обход файлов строго по правилам `.gitignore` без замусоривания графа артефактами сборки `target/`, `node_modules/`, `.git/`).
- **Реактивная автоматизация (Фаза 18)**:
  - Фоновый `ProjectWatcher` на базе `notify`: автоматический ре-скан при сохранении исходного кода в IDE с дебаунсом 500 мс.
  - Установка Git-хуков (`post-commit`, `post-merge`, `post-checkout`) через `ob2h project hook install`.
  - Фоновый `AutoSyncWorker`: автоматическая периодическая репликация с VPS по расписанию.
  - Команда `ob2h doctor [--fix]`: глубокая диагностика БД, FTS5, Candle, 7 AI-агентов, Git-хуков и автоматический фикс.
- **Семантический поиск по коду и Протокольные возможности MCP (Фаза 19)**:
  - Векторное индексирование узлов кода моделью Candle MiniLM 384d (`embed_unembedded_nodes`).
  - Гибридный поиск по кодовому графу `project_graph_search` в режимах `hybrid` (RRF $k=60$), `text`, `vector`.
  - Реализация нативных MCP Resources (`resources/list`, `resources/read`): `project://current/overview`, `project://current/god-nodes`, `project://current/schema`, `memory://context`.
  - Реализация нативных MCP Prompts (`prompts/list`, `prompts/get`): `explain_component`, `plan_feature`.
- **Графовая архитектурная аналитика и Безопасность рефакторинга (Фаза 20)**:
  - Новый 25-й MCP-инструмент `project_impact`: анализ радиуса изменений (Blast Radius), BFS-обход обратных зависимостей, дерево потребителей и оценка риска (`Low`, `Medium`, `High`).
  - Детектор циклических зависимостей алгоритмом Тарьяна (Tarjan's SCC) в графе проекта.
  - Метрики связанности и стабильности компонентов Роберта Мартина ($C_a, C_e, I$).
  - 50 всеобъемлющих интеграционных тестов Rust (`cargo test`).

## [1.1.0] — 2026-09-01

### Added
- **Поддержка языков PHP, Dart и Java в AST-графе кода** (`src/project/ast.rs`):
  - PHP (`.php`): парсинг `class`, `interface`, `trait`, функций/методов, `use`-импортов, `extends`, множественного `implements`.
  - Dart / Flutter (`.dart`): парсинг `class`, `mixin`, функций/методов, `import`/`export`, `with`, `implements`.
  - Java (`.java`): парсинг `package`, `import`, `class`, `interface`, `enum`, `record`, методов и связей наследования/реализации.
  - Полный набор юнит-тестов для новых парсеров в `tests/test_ast_extractor.rs`.

## [1.0.0] — 2026-08-30

### Fixed
- **Плагин Hermes: автоподстановка ключа агента в `ob2h serve`** (`plugin/ob2h/__init__.py`).
  Раньше плагин спавнил ob2h с `OB2H_DATA_DIR`, но без `OB2H_LLM_API_KEY` — процесс от
  плагина оставался без ключа, и дриминг/LLM-инструменты падали с
  `401 Authentication Fails (governor)`. Теперь плагин сам резолвит и прокидывает
  `OB2H_LLM_API_KEY/OB2H_LLM_MODEL/OB2H_LLM_BASE_URL` в дочерний процесс: приоритет —
  окружение Hermes > ob2h.json > `.env` агента ($HERMES_HOME/.env, ключ по конвенции
  DEEPSEEK_API_KEY). Юнит-тесты: 4 новых на `_llm_child_env` (20 всего).

### Added
- **Проектная память и детерминированный AST-граф кода** (Фазы 10-13, вдохновлено Graphify):
  - Таблица `projects` в SQLite (миграция M3) с поддержкой изоляции памяти и сущностей по `project_id`.
  - Высокопроизводительный статический AST-парсер (`AstCodeExtractor`) для Rust, Python, TypeScript/JavaScript, Go, SQL. Извлекает классы, структуры, интерфейсы, трейты, функции, таблицы и связи (`DEFINES`, `IMPORTS`, `INHERITS`, `FOREIGN_KEY_TO`) со 100% точностью (`provenance: 'ast'`, `confidence: 1.0`) без расхода токенов LLM.
  - Графовая аналитика и выявление центральных узлов («God Nodes») по Degree & In-degree Centrality.
  - Сжатые архитектурные дайджесты (`project_report`) и блок `<project_context>` для системного промпта агента.
- **5 новых MCP-инструментов** (всего 24 инструмента):
  - `project_init` — регистрация и настройка проекта.
  - `project_scan` — детерминированное AST-сканирование кодовой базы и индексация в граф.
  - `project_context` — генерация архитектурного контекста с God Nodes для промпта агента.
  - `project_graph_search` — фильтрованный поиск по графу кода с указанием файлов и строк.
  - `project_report` — подробный Markdown-дайджест кодовой базы.
- **Мультиагентный установщик и CLI-менеджер (`ob2h agent`)**:
  - Поддержка подключения в 1 клик для всех популярных AI-ассистентов: Claude Code, Cursor, Windsurf, ZCode, Gemini CLI / Antigravity, Qwen Code, OpenCode, Hermes.
  - Команды `ob2h agent install [--agent <name>|--all] [--path <project_dir>]` и `ob2h agent status`.
  - Команды `ob2h project init/scan/list/report`.

## [0.9.0] — 2026-08-23

### Added
- **Синхронизация двух инстансов PC ↔ VPS** (ADR-9): инкрементальные gzip-бандлы JSONL
  поверх SSH/manual, `ob2h sync status/export/import/apply-inbox/push/pull`.
  LWW по `updated_at` + tie-break по приоритету `origin`, tombstones, идемпотентность
  по bundle_id, авто-бэкап перед каждым новым бандлом, эмбеддинги в бандле
  (или re-embed локальной моделью). Фаза автодрима `after_dream` (best-effort).
- **Миграция M2** (аддитивная): `origin`/`deleted_at`/`updated_at` +
  `sync_state`; авто-бэкап `pre-v08-*.db` перед миграцией живой БД; даунгрейт-безопасно.
- **Tombstones**: `memory_forget`/`purge_weak` реплицируют удаление, поиск и контекст
  фильтруют удалённое, физическая чистка — в maintenance автодрима (retention×2).
- **CLI `ob2h skill install`**: скилл из репо (`skills/ob2h/`) деплоится в Hermes
  с подстановкой путей машины (единый исходник Windows/Linux).
- Обвязка синка: systemd-таймер (`scripts/vps/`), Task Scheduler-скрипт
  (`scripts/pc/`), образец `peers.json`.
- SQLite `busy_timeout`, `origin=''`-семантика («строка этого узла»).

### Changed
- AGENTS.md/docs приведены к Rust-реальности; HERMES_INTEGRATION.md переписан
  (режимы 0/A/B, синк, обновление); ARCHITECTURE.md актуализирован (плагин, sync,
  M2-схема, 19 инструментов); новый гайд `docs/SYNC.md` (кому нужно, настройка,
  конфликты, безопасность); README — раздел синхронизации и сценарии.

## [0.8.0] — 2026-08-23

### Added
- **MCP-инструмент `session_ingest`** (19-й, в конец списка): массовая запись транскрипты
  сессии парами user/assistant в daily-лог с дедупом по `(session_id, позиция сообщения)`
  (kv-счётчик) — повторный вызов с полной транскриптой добавляет только хвост. Роли кроме
  user/assistant пропускаются. Контракт старых 18 инструментов не изменился (снапшот-тест
  `tools/list`).
- **MemoryProvider-плагин для Hermes** (`plugin/ob2h/`, Python stdlib-only): долгоживущий
  subprocess `ob2h serve` + JSON-RPC/stdio. Автоматически, без инициативы модели:
  `sync_turn` → `session_ingest` каждый ход; `on_session_end`/`on_pre_compress` → полная
  транскрипта; `queue_prefetch`/`prefetch` → инъекция `<agent_memory>` перед ходом
  (+ `recall_status` 🧠); `get_tool_schemas` → инструменты ob2h (кроме автоматических
  session_log/session_ingest/memory_search/memory_context); `on_memory_write` — зеркало
  builtin-памяти. Non-primary контексты (cron/subagent) не пишут. Рестарт subprocess
  с backoff, health-ping 60с, деградация без падения агента.
- **CLI `ob2h plugin install|uninstall|status`**: деплой плагина в `$HERMES_HOME/plugins/ob2h/`,
  `ob2h.json` пинит binary/data_dir; конфиг Hermes не правится — печатает сниппет
  `memory.provider: ob2h` для ручной вставки.
- SQLite `busy_timeout=5000` (Mode B: плагин + mcp_servers одновременно).

### Tests
- Rust: session_ingest (пары/дедуп/хвост/ошибки контракта), снапшот tools/list.
- Python: 16 тестов — RPC-клиент против фейк-сервера (handshake/таймаут/рестарт),
  провайдер (аккумулятор, gating, схемы, prefetch), интеграция с реальным `ob2h serve`.

## [0.1.1] — 2026-08-18

### Added
- **dream-extract**: во время дрима сущности и отношения извлекаются из новых
  записей сессий в общий граф (`OB2H_DREAM_EXTRACT_ENABLED`, по умолчанию вкл).
  Сессии и документы теперь populate один граф с дедупом по label|type.
- **Локальные эмбеддинги**: fastembed 0.8.0 установлен и проверен на Python 3.14;
  модель `paraphrase-multilingual-MiniLM-L12-v2` (0.22 ГБ, 384d) скачана и
  протестирована на русском (semantic-тест в `tests/test_embedding_local.py`,
  маркер `embeds`). Дефолт конфига исправлен с недоступного multilingual-e5-small.
- Документированы альтернативы эмбеддингов: LM Studio embeddinggemma-300m-qat
  (уже скачана у владельца) и Ollama mxbai-embed-large через `OB2H_EMBED_PROVIDER=api`.

## [0.1.0] — 2026-08-18

Первая рабочая версия: локальное MCP-хранилище знаний для Hermes (stdio).

### Added
- **Ядро хранения** (`config/db/vector/embedding`): SQLite WAL с версионными
  миграциями, FTS5-trigram (русский из коробки), вектора BLOB+numpy с косинусным
  поиском, провайдеры эмбеддингов fastembed (CPU, без torch) и OpenAI-совместимый API.
- **Память**: гибридный поиск FTS+вектор со слиянием RRF k=60, важность с затуханием
  и очисткой слабых, блок `<agent_memory>` для промпта (порт MemoryService).
- **Workspace**: MEMORY/SOUL/USER.md, history.jsonl с курсорами, git-история правок
  с восстановлением (порт MemoryStore + GitStore).
- **Консолидация сессий**: триггер по бюджету токенов, суммаризация LLM с raw-фолбэком,
  инструмент `session_log` (порт Consolidator).
- **Граф знаний KAG-lite**: чанкинг 3000/300 по предложениям, LLM-экстракция
  сущностей/отношений, инференс отношений по описаниям (~44 шаблона), фильтр мусора,
  дедуп-апсерт, гибридный поиск с 1-hop соседями, `graph_reason` с confidence
  (порт OneKE + KAGReasoningService, PG-путь).
- **Дриминг**: двухфазный Dream (анализ → агентный цикл точечных правок MD ≤10 итераций
  → git-коммит), AutoDreamWorker с гейтами 4ч/10 событий/lock(stale 1ч) и ретеншном
  daily-логов (порт Dream + AutoDreamWorker).
- **MCP-инструменты** (19): memory_save/search/update/forget/context,
  workspace_read/write, session_log, knowledge_extract, graph_search/reason/stats,
  dream_run/status/log/restore, omnes_stats/backup.
- **Служебное**: бэкапы VACUUM INTO + workspace с ротацией 14, CLI
  (`python -m ob2h.dream_cli run|status|backup`), логирование с ротацией.
- 102 теста (юнит + интеграционные через живой stdio MCP-клиент), ruff чисто.

## [0.0.1] — 2026-08-18

### Added
- Каркас проекта: план разработки, правила, docs, codegraph, pyproject.
