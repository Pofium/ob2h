# Tasks: ob2h v2.0 Core & Lifecycle (`v2-core`)

## Фаза 72 — FastEmbed & Batch Vector Pipeline
- [x] T-72.1 Оптимизация базового Candle: вынос синхронного инференса в `spawn_blocking` и пакетная обработка чанков (батчи 32/64).
- [x] T-72.2 Опциональный крейт `fastembed` / `ort` под флагом `onnx`: INT8-модели мультиязычного/кодового класса (`multilingual-e5-small`; альтернативы `jina-embeddings-v2-base-code`, `nomic-embed-text-v1.5`).
- [ ] T-72.2a AST-aware чанкинг для векторизации кода: нарезка по границам tree-sitter-узлов с заголовком `// File: <путь> | Scope: <контекст>`, прирост Recall@10 в bench.
- [ ] T-72.2b Асимметричные префиксы моделей в трейте `EmbeddingProvider`: поля `query_prefix`/`document_prefix` с авто-подстановкой и проверкой дельты RU-триплетов в bench.
- [ ] T-72.3 Механизм `EmbeddingFingerprint`: фиксация `model_id:backend:quant:dim:prefix_vN` в метаданных БД и блокировка смешанного поиска (триггер `reindex --embeddings`).
- [ ] T-72.4 CLI-команда `ob2h reindex --embeddings`: фоновая пакетная переиндексация векторов при смене бэкенда/модели с прогресс-баром.
- [ ] T-72.5 Изоляция пула воркеров (`EmbeddingWorkerPool`): выделенный семафор и потоки, Stale-While-Revalidate (`degraded_mode: "warming_up_embeddings"`).
- [ ] T-72.6 Безопасность: санитайзер текста перед векторизацией (удаление секретов, ключей API и токенов).
- [ ] T-72.7 CLI-команда `ob2h bench embeddings`: замер скорости инференса и проверка качества триплетов с отдельным порогом на RU и RU/EN-смешанных триплетах.

## Фаза 72b — Production Vector Index (vec0 & SIMD)
- [ ] T-72b.1 Инкрементальный upsert векторов в `vec0` (opt-in `OB2H_VEC0`) в `BEGIN IMMEDIATE`.
- [ ] T-72b.2 Двухступенчатый конвейер извлечения: ступень 1 — mean-centered Binary Quantization (BQ) + Hamming `popcnt` oversample; ступень 2 — точный FP32/INT8 rescore.
- [ ] T-72b.3 Партиционирование по пространствам имен: раздельные векторные индексы для `memories` и `ast_nodes` с фильтрацией по `project_id`.
- [ ] T-72b.4 Регрессионный гейт: проверка recall@10 векторного поиска против brute-force эталона (ADR-13).
- [ ] T-72b.5 Content-addressed кэш (blake3): таблица `file_blobs` для мгновенного переключения веток без шторма ре-парсинга/ONNX.

## Фаза 73 — Usage-Weighted Personalized PageRank (U-PPR)
- [ ] T-73.1 Структура `GraphAdjacency`: компактное разреженное представление подграфа в памяти с весами рёбер.
- [ ] T-73.2 Реализация формулы $W(e)$ в `src/graph/pagerank.rs` (CAP_access=3.0, lambda_code=0 vs lambda_memory>0, trust_score, In-Degree Penalty, Grace period, Mute Node).
- [ ] T-73.3 Буферизация телеметрии обращений + обслуживание WAL (`PRAGMA wal_checkpoint(TRUNCATE)` + `PRAGMA optimize` в autodream и shutdown).
- [ ] T-73.4 Экстракция $k$-hop сабграфа (до 5k–10k узлов) с взвешиванием `IDF * trust_score` и Active Working Set Boost (`git diff HEAD`).
- [ ] T-73.5 Фильтрация устаревших данных: узлы с флагом `SUPERSEDED` исключаются из целевого вектора PPR.
- [ ] T-73.6 Сквозная телеметрия (`memory_context`, `graph_reason`, prefetch, `project_impact`) + Mute Node (`trust_score *= 0.3`).
- [ ] T-73.7 Калибровка весов на multi-hop golden наборе (сравнение выдачи с базовым PPR в bench).
- [ ] T-73.8 Токен-бюджетный AST-скелет: параметр `max_tokens` в `graph_reason`/`memory_context` + Elbow/Drop-off Cutoff.
- [ ] T-73.9 Sub-token декомпозиция идентификаторов в FTS5 (CamelCase/snake_case).

## Фаза 74 — Автономное разрешение конфликтов и актуализация (Dream)
- [ ] T-74.1 Трехуровневая классификация конфликтов: Temporal Supersession, User Correction, Contextual Coexistence.
- [ ] T-74.2 Предварительная фильтрация кандидатов: блокинг по категориям + Entity/Subject Overlap + cosine > 0.82.
- [ ] T-74.3 Детектор и синтезатор противоречий: Online (`llm_client`) и Offline (эвристики без сети).
- [ ] T-74.4 Формирование рёбер `SUPERSEDES` и `CONTRADICTS`, `valid_until`, tombstone-статусы, `git_head_sha` (git-anchored facts).
- [ ] T-74.5 Режим Human-In-The-Loop: инструмент `knowledge_reconcile` с предпросмотром для важных сущностей.
- [ ] T-74.6 Бюджет ночного цикла: жесткое ограничение на количество токенов и время работы Dream.
- [ ] T-74.7 Контроль идемпотентности графа и автоматический откат транзакции при ухудшении поисковых метрик (ADR-13).

## Фаза 75 — Интеграция цикла агента и наблюдаемость
- [ ] T-75.1 Автоматическая инвалидация устаревших findings Ralph при смене AST-хешей/сигнатур + `git_head_sha`.
- [ ] T-75.2 Наблюдаемость: `tracing` spans (`embed`, `parse`, `ppr`, `fts`, `vec`, `dream`, `wal_checkpoint`) и интеграция метрик в `omnes_stats`.
- [ ] T-75.3 Синхронизация v2: поддержка рёбер `SUPERSEDES`/`CONTRADICTS` в sync-бандлах и CRDT `max` для счётчиков обращений.

## Фаза 76 — Кроссплатформенный Lifecycle (cargo-dist), self-uninstall и упаковка
- [ ] T-76.1 Движок предварительной диагностики (`Pre-flight Diagnostics Engine`: CPU AVX2/NEON, VC++ только при onnx, конфликтующие процессы).
- [ ] T-76.2 Упаковка через `cargo-dist` (`dist.toml`) + опциональный шаблон Inno Setup.
- [ ] T-76.3 macOS: Universal2 через cargo-dist + документация Gatekeeper.
- [ ] T-76.4 Linux: статический musl `.tar.gz`, `.deb`/`.rpm`, `curl | sh`.
- [ ] T-76.5 Пользовательские демоны: Task Scheduler `ob2h-autodream` (Windows), LaunchAgent (macOS), `systemd --user` (Linux).
- [ ] T-76.6 Zero-Touch IDE & Agent Auto-Discovery: JSONC-safe идемпотентный патч, shell completions, `PATH`.
- [ ] T-76.7 Предзагрузка моделей (First-run Model Hydration) с прогресс-баром.
- [ ] T-76.8 Встроенная подкоманда `ob2h self uninstall [--keep-data | --purge-data] [--silent]` (Full Teardown Engine + `self-replace`).
- [ ] T-76.9 Управление данными и аварийная очистка: `--keep-data`/`--purge-data` и расширение `ob2h doctor --fix`.

## Фаза 77 — Финализация, CI/CD релизная матрица, документация и запуск
- [ ] T-77.1 Настройка кроссплатформенного релизного конвейера в GitHub Actions (сборка, SHA256, SBOM, optional gated signing).
- [ ] T-77.2 Утилита `ob2h migrate --dry-run`: предпросмотр миграции M12 с авто-бэкапом.
- [ ] T-77.3 Оформление архитектурных решений ADR-24..29 в `PLAN.md`.
- [ ] T-77.4 Актуализация всей пользовательской документации: `README.md`, `ARCHITECTURE.md`, `docs/SYNC.md`, `docs/INSTALL.md`, `skills/ob2h/SKILL.md`.
- [ ] T-77.5 Публикация мажорного релиза v2.0.0.
