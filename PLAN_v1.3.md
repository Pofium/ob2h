# 🚀 План разработки OB2H v1.3: Обучаемая память, честный prefetch, лёгкая база

> **Версия плана:** 1.3.0
> **Статус:** Утверждён к реализации (владельцем, 2026-09-13); выполняется по порядку фаз
> **Обновлён:** 2026-09-13 — добавлены трек B «Ralph Knowledge Layer» (Фазы 26–28), опциональная Фаза 29 (PPR), исследование технологий с источниками (§8), счётчик LWW-конфликтов (25.4); **rev.2** — ревью владельца: OMEGA/mcp-memory-service/GAAMA в §8, расширяемые kind в memory_links (23.5), dual-seed/edge-type PPR (29), sqlite-vec-rescore как явный кандидат v1.4
> **Предыдущие этапы:** v0.8/0.9 (Ядро, Память, Дриминг, Синк), v1.0/1.1 (AST-граф 8 языков, God Nodes, мультиагентность), v1.2 (Zero-Config проекты, инкрементальный AST, AutoSync, семантика кода)
> **Источники:** Ralph-спека `Ralph Knowledge Layer (ob2h)_v1.2.3.md` (нормативная для Фаз 26–28), аудит живой системы 2026-09-13 (§1), предложения владельца, интернет-исследование 2026-09-13 (§8)
> **Оценка:** трек A (Фазы 21–25) ≈ 7 дней; трек B (Фазы 26–28) ≈ 5.5–8 дней; Фаза 29 ≈ 1–2 дня
> **Принцип совместимости:** 100% обратная совместимость (Zero Breaking Changes) для всех 25 существующих инструментов MCP. Все изменения контракта — аддитивные (см. §5), с записью в `CHANGELOG.md`.

---

## 1. Мотивация (замеры 13.09.2026, живая система)

| Наблюдение | Факт | Следствие |
|---|---|---|
| `build_context` не использует гибридный поиск | `ORDER BY importance DESC LIMIT 100` + подстрочный overlap (`0.6*importance + 0.4*overlap`) | RRF-пайплайн (FTS5 trigram + cosine) работает только в явном `memory_search`; автоконтекст каждого хода его игнорирует |
| Prefetch-блок без бюджета | Наблюдались блоки 17–20К символов | Hermes обрезает head/tail (spill в `hook_outputs`) — теряется середина контекста |
| `touch_access` вызывается только из `memory_search` | `service.rs`: единственный вызов | Записи, инжектируемые prefetch'ем, не считаются использованными; `decay_importance(0.01)` в autodream гасит записи, реально живущие в каждом контексте |
| `relations=0` | memories=283, граф документов — 726К рёбер | У памяти нет связного слоя: search не расширяется на соседей |
| БД 939 884 КБ | graph_nodes=604 920 × f32-эмбеддинг 384d (1536 Б) ≈ 90% объёма | Полный VACUUM INTO × ротация 14 копий ≈ до 13 ГБ на диске |
| Fallback `FakeEmbedding` молчит | `embedding/mod.rs`: при падении загрузки модели — тихая подмена | Векторы становятся мусором, search выглядит рабочим |
| `sync_turn` плагина не принимает `turn_author` | Hermes шлёт его провайдерам с расширенной сигнатурой | В общих gateway-сессиях авторы смешиваются в одной памяти |

**Порядок выполнения обязателен:** Фаза 21 (bench) — до Фазы 22, иначе изменение score-формулы нечем измерить. Фазы 26–28 (Ralph) независимы от 22–25 и могут идти параллельно или после; Фаза 29 — опциональна. Фаза 21 — всегда первой.

---

## 2. Архитектурный обзор v1.3

```
                    ┌──────────────────────────────────────────────┐
                    │        ПИШУЩИЙ КОНТУР (как в v1.2)           │
                    │  session_ingest → consolidator → dream       │
                    └──────────────────┬───────────────────────────┘
                                       │
            ┌──────────────────────────▼───────────────────────────┐
            │                ОБУЧАЮЩИЙ КОНТУР (новое)              │
            │  touch_access (prefetch+search) → trust ↑            │
            │  dream-ревизия: confirmed/outdated → trust ±         │
            │  memory_feedback (агент, вручную) → trust ±          │
            │  decay_importance → trust ↓ (без автоудалений)       │
            ├──────────────────────────────────────────────────────┤
            │              ЧЕСТНЫЙ PREFETCH (новое)                │
            │  build_context: гибрид RRF (как memory_search)       │
            │  score = hybrid + importance + trust + recency+access│
            │  MMR-диверсификация · бюджет 8К символов             │
            ├──────────────────────────────────────────────────────┤
            │                     SQLite (data/ob2h.db)            │
            │  M5: memories.trust, memory_links, embedding v2 i8   │
            └──────────────────────────────────────────────────────┘
                    ob2h bench (golden set, recall@k, MRR)
                          — регрессионный контур —
```

**Трек B — Ralph Knowledge Layer (Фазы 26–28):** циклы разработки агента —
`ralph_runs`/`ralph_iterations`/`ralph_findings` + `ast_changes` (миграция **M6**),
авто-вердикты только по объективным сигналам (exit-code тестов), `ralph_context`
с reuse-кандидатами из AST-графа, staleness-pass по AST-дельтам, debt-леджер,
dream-фаза 3, скилл `ralph-loop`. Спецификация — `Ralph Knowledge Layer (ob2h)_v1.2.3.md`.

---

## 3. Модель данных: миграция M5

### Расширение `memories`
```sql
ALTER TABLE memories ADD COLUMN trust REAL NOT NULL DEFAULT 0.5;
ALTER TABLE memories ADD COLUMN last_feedback_at TEXT;
```
`trust` — машинный сигнал «запись подтверждена использованием/ревизией», отделён от
пользовательского `importance` (0–1, авторский). Итоговый скоринг использует оба.
Флаг `candidate_for_forget` — в существующем JSON `meta` (новая колонка не нужна).

### Таблица `memory_links`
```sql
CREATE TABLE IF NOT EXISTS memory_links (
    from_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    to_id   INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    kind    TEXT NOT NULL,            -- same_project | entity | category | manual
    weight  REAL NOT NULL DEFAULT 1.0,
    created_at TEXT NOT NULL,
    PRIMARY KEY (from_id, to_id, kind)
);
CREATE INDEX IF NOT EXISTS idx_memlink_from ON memory_links(from_id);
CREATE INDEX IF NOT EXISTS idx_memlink_to   ON memory_links(to_id);
```

### Формат эмбеддинга v2 (int8)
- Старый: f32 × dim = 1536 Б (384d). Новый: `[scale: f32][i8 × dim]` = 388 Б.
- Форматы различимы однозначно по длине блоба; чтение — обратно совместимо, запись — всегда v2.
- **sqlite-vec не используем** (ADR-2: бэклог), квантование — собственным кодом в `src/vector/`.

### Golden set bench
`data/bench/golden.jsonl` — **вне БД**, строки `{"query": "...", "expect_keys": ["hmem-..."], "note": "..."}`.
Содержит персональные данные → в `.gitignore` (весь `data/` игнорируется); в репозиторий —
синтетический `tests/fixtures/golden_synthetic.jsonl` для юнит-тестов метрик.

**Нумерация миграций:** M5 в этом плане занята (trust, memory_links, embedding v2).
Ralph-миграция из спеки (названа там «M4», что уже устарело) в репозитории выполняется
как **M6** (версия схемы v7) — см. Фазу 26.

---

## 4. Детальные фазы реализации

### Фаза 21 — `ob2h bench`: golden set и recall@k (Оценка: 1 день)

Цель: регрессионный контур для всех последующих твиков retrieval. Без него п. 22 — стрельба вслепую.

- [x] **21.1** CLI `ob2h bench [--mode search|context] [--k 5,10] [--json]`:
  - читает `data/bench/golden.jsonl`, прогоняет каждый запрос через `memory_search` / `build_context`;
  - метрики: recall@k (по `expect_keys`), MRR, p50/p95 latency; вывод — таблица + markdown-блок для `CHANGELOG.md`.
- [x] **21.2** Наполнение: 30–40 запросов по живой памяти — разговорные русские формулировки
  («как чинить кириллицу в коннекторах», «куда едет сторе VPS»), синонимы, опечатки;
  `expect_keys` — реальные ключи `hmem-*`. (36 кейсов)
- [x] **21.3** Детерминизм в тестах: bench на фиксированных векторах-заглушках (§7 AGENTS.md),
  без сети и без fastembed; реальный прогон — вручную на живой БД.
- [x] **21.4** Фиксация baseline: результат bench до любых изменений — в `docs/bench_baseline.md`.

### Фаза 22 — Честный prefetch (Оценка: 1.5 дня)

Цель: автоконтекст каждого хода использует лучший поисковый пайплайн и влезает в бюджет.

- [x] **22.1** Гибрид в `build_context(query)` (`src/memory/service.rs`):
  непустой query → тот же RRF-пайплайн, что `memory_search` (FTS5 trigram + cosine, k=60);
  пустой/тривиальный запрос → fallback `importance DESC` (как сейчас).
- [x] **22.2** Формула скоринга:
  `score = 0.35*hybrid_rank + 0.25*importance + 0.2*trust + 0.1*recency + 0.1*log1p(access_count)`;
  `recency = exp(-age_days / half_life)`, `OB2H_HALF_LIFE_DAYS=90`.
  До включения trust (Фаза 23) слагаемое 0.2*trust = константа 0.1 — формула уже финальная.
- [x] **22.3** MMR-диверсификация (λ=0.7 по эмбеддингам): топ по важности больше не вытесняет
  другие категории пятью вариациями одного правила.
- [x] **22.4** Бюджет блока: `OB2H_PREFETCH_MAX_CHARS` (дефолт 8000) — резать по record-границам
  с хвоста, а не отдавать 20К на head/tail-обрезку Hermes.
  `memory_context` получает опциональный `max_chars`; `max_tokens` сохранён (совместимость).
- [x] **22.5** `touch_access` для записей, реально попавших в блок (сейчас — только из search).
- [x] **Тесты 21→22:** юнит-тесты метрик; `tests/test_context_v2.rs` — бюджет по record-границам
  с маркером, touch только вошедших, fallback без запроса, детерминизм fallback; bench-прогон
  на живой БД: search без изменений (0.222/0.198), context 0.542/0.583 — падение против
  смещённого importance-baseline ожидаемо и разобрано в `docs/bench_baseline.md`
  (новый ориентир: search recall@10 ≥ 0.5 силами Фаз 25/29).

### Фаза 23 — Trust и feedback-петля (Оценка: 2 дня)

Цель: память учится на использовании; устаревшее гаснет управляемо, без автоудалений.

- [x] **23.1** `touch_access` → `trust = min(1.0, trust + 0.02)`, `last_feedback_at`;
  `decay_importance(rate)` в autodream → также `trust *= (1 - rate)`.
- [x] **23.2** Dream-ревизия: в промпт дрима добавлять блок «ревизия памяти» — N записей
  с наименьшим `trust`/давним `last_feedback_at`; LLM-вердикт `confirmed | outdated | contradicted`
  → `trust +0.1 / −0.4 / −0.5`; вердикты — в dream-отчёт и git-коммит workspace.
- [x] **23.3** Никаких автоудалений (правило №1 пользователя): при `trust < 0.15` запись получает
  `meta.candidate_for_forget=1`, видна в `dream_status`/дрим-отчёте; удаление — только явный
  `memory_forget`.
- [x] **23.4** Новый MCP-инструмент **`memory_feedback` (№26)**:
  `memory_feedback(key, verdict: helpful|unhelpful|outdated, note?)` → trust `+0.15/−0.2/−0.3`,
  запись в `meta.feedback`; регистрируется в плагине Hermes (agentic feedback из чата).
- [x] **23.5** Автосвязи `memory_links`: при `memory_save` — связи `same_project`/`category` +
  пересечение сущностей (существующий OneKE-lite-экстрактор); `memory_search` mode=hybrid
  опционально (`related=true`) добавляет 1-hop соседей отдельным блоком `[related]`,
  не смешивая с основными хитами. Набор `kind` расширяемый без ломки API: в v1.3 —
  `same_project|entity|category|manual`; зарезервировать `contradicts|causes|supersedes`
  для dream-ревизии (23.2; belief-derivation — бэклог, см. mcp-memory-service в §8).
  *Отступление (записано в PLAN.md §6): entity-связи в v1.3 детерминированных kinds нет —
  LLM-экстрактор на каждом save давал бы токены на каждый ход; kind=entity зарезервирован.*
- [x] **23.6** Дедуп со builtin-памятью Hermes: в промпт дрима — правило «если правило уже есть
  в ob2h, не предлагать его в builtin MEMORY.md» (сейчас часть правил живёт в обоих сторах
  и попадает в контекст дважды).
- [x] **Тесты:** детерминированные вердикты FakeLLM; trust-клампы 0..1; `candidate_for_forget`
  не удаляет запись; memory_links не дублируются, каскад при forget.

### Фаза 24 — Лёгкая база: квантование и бэкапы (Оценка: 1.5 дня)

Цель: 939 МБ → ~300 МБ; бэкап памяти — быстрый и проверяемый.

- [x] **24.1** Запись эмбеддингов в формате v2 (§3) в `src/vector/` + `src/embedding/`:
  чтение старых f32-блобов без миграции, запись — всегда v2.
  *(реализация: magic-байт `0x01` + `[scale f32][i8 × dim]` — одной длины блоба
  недостаточно: 388 = 4×97; dual-read в `deserialize`, legacy-парсер `deserialize_f32`)*
- [x] **24.2** Разовая конвертация: `ob2h db quantize-embeddings [--dry-run]` — батчевое
  переписывание `embedding` для `graph_nodes`, `memories`, `chunks`; прогресс и отчёт
  (до/после, размер файла). Ожидание: ~605K узлов × 1.1 КБ экономии ≈ −670 МБ.
  *(проверено на копии живой БД: 963 → 619 МБ, −36%; проекция −670 МБ не достигнута —
  у ~269K узлов графа эмбеддинги пустые (X''), реальные векторов 199K. На живой БД —
  после рестарта сервера на новый бинарник, одной командой)*
- [x] **24.3** `omnes_backup --scope full|quick` (дефолт `full` — как сейчас):
  `quick` = ATTACH-копия `memories`/`kv`/session-таблиц + workspace-архив (~10–30 МБ);
  FTS восстанавливается внешне-контентными триггерами при INSERT.
  Ротация раздельная: `OB2H_BACKUP_KEEP_FULL=3`, `OB2H_BACKUP_KEEP_QUICK=14`.
  *(CLI: `ob2h backup --scope quick`; quick = memories+kv+memory_links + workspace)*
- [x] **24.4** `ob2h backup verify <path>`: открыть копию, `PRAGMA integrity_check`,
  сверить счётчики (memories, graph_nodes) с живой БД, отчёт. Бэкап, который ни разу
  не восстанавливали, — не бэкап.
- [x] **Тесты:** roundtrip f32→i8→f32 (макс. искажение косинуса ≤ 0.01 на фиксированных
  векторах); восстановление quick-бэкапа во временный каталог; verify детектит битую копию;
  bench до/после квантования — без деградации recall@5. *(search 0.222/0.306/MRR 0.198 —
  идентично; context 0.569/0.611 — в пределах шума; `tests/test_quantize.rs`)*

### Фаза 25 — Надёжность и мультиагентность (Оценка: 1 день)

- [x] **25.1** `FakeEmbedding` — громкий: warn при активации; `omnes_stats` → поле
  `backend=fake|local_bert|api`; `memory_search` при fake добавляет
  строку-предупреждение в выдачу; секция в `ob2h doctor` (реальная канареечная
  проверка: загрузка модели + embed, красный статус при деградации).
- [x] **25.2** `plugin/ob2h/__init__.py`: `sync_turn(..., turn_author="", **kwargs)`
  (Hermes шлёт провайдерам, чья сигнатура его принимает); автор — в `meta.author`
  daily-записей (`session_log`/`session_ingest` + опциональный `author` в контракте
  обоих инструментов); `memory_context` + `author` — записи с чужим `meta.author`
  исключаются из prefetch-блока; плагин передаёт автора в prefetch и запись.
- [ ] **25.3** (Опционально, только после bench) реранкер через fastembed `TextCrossEncoder`
  (bge-reranker-base, ONNX — в рамках ADR «без torch/transformers»): за флагом
  `OB2H_RERANK=1`, реранк top-30 → top-8 в `memory_search`; по умолчанию выключен.
  *Отложено: fastembed-rust тянет ort (тяжёлая зависимость, ADR-14 в §8) — решение
  отдельно по §6 PLAN.md; альтернатива без зависимостей — LLM-реранк через llm_client.*
- [x] **25.4** Наблюдаемость синка: `sync status`/import-отчёт — счётчик `конфликтов LWW
  проиграно (всего)` (kv `sync.conflicts_total`, копится на каждом import).
- [x] **Тесты:** фейковый бэкенд детектится в stats/doctor; `turn_author` пишется и
  фильтруется (записи с meta.author); плагин остаётся stdlib-only; python-контракт
  обновлён (26 инструментов, изоляция env в TestLlmChildEnv).

---

### Фаза 26 — Ralph-ядро: циклы, итерации, AST-дельта (Оценка: 2–3 дня)

Нормативная спека — `Ralph Knowledge Layer (ob2h)_v1.2.3.md` (§4–§5, FR-K1…K12, ADR-K1…K6).
Миграция в спеке названа «M4» (устарело: M4 занята project_files из v1.2, M5 — trust/memory_links
из этого плана) — в репозитории выполняется как **M6** (версия схемы v7).

- [x] **26.1** M6 (аддитивно; пре-бэкап `pre-m6-*.db`; **graph_nodes не пересоздавать** —
  ручная колонка `confidence`): `ralph_runs`, `ralph_iterations`, `ralph_findings`,
  `ast_changes` + индексы (DDL — спека §4.2). Тест схемы на наличие `confidence`.
- [x] **26.2** `ralph_start` / `ralph_iteration` / `ralph_verdict` (спека §5): авто-вердикт
  только по `tests_summary` — `failed==0 и passed>0 → verified`, `failed>0 → failed`,
  нет → `unconfirmed`; `self_assessment` НИКОГДА не влияет (ADR-K4); идемпотентность по
  `(run_id, task_id, n)`.
- [x] **26.3** AST-рескан на `ralph_iteration` через существующий `ProjectService` → diff
  по каноническим сигнатурам узлов → `ast_changes` (ADR-K3; git-дифф не используется).
- [x] **26.4** Тесты: миграция на копии живой и чистой БД; сценарий 3 итераций
  (2 красные → 1 зелёная) → верные вердикты; повторный `ralph_iteration` не дублирует.
  *(M6 уже применён к живой БД при деплойных прогонах — аддитивно; плюс тест
  `m6_preserves_confidence_column`)*

### Фаза 27 — Ralph-контекст и стейлнесс (Оценка: 2–3 дня)

- [x] **27.1** `ralph_context(run_id, task_id, max_tokens?, mode?)`: приоритеты 1–5 спеки §6 —
  фрагмент спеки из `openspec/changes/<slug>/`, негативный опыт (failed/overturned findings),
  **reuse-кандидаты** через `project_graph_search` (FR-K4), инварианты, архитектурная зона
  (`project_context`/`project_impact`); lite = 1+2+2.5 ×0.5; лимит 6000 токенов; пакет
  выгружается в `data/ralph/contexts/<run_id>/` + `context_ref` (путь+sha256).
  *(reuse-кандидаты в v1: детерминированный топ по is_god_node/val вместо векторного
  семпоиска — семантический режим подключается в v1.4 Ф33)*
- [x] **27.2** Staleness-pass (FR-K5): для `ast_changes` modified/removed — пре-фильтр LIKE +
  точный JSON-матчинг по `symbols` → `ralph_findings.verdict='stale'`; повторный вызов не перемечает.
- [x] **27.3** Debt-семантика (FR-K6): findings `kind=deferred` с meta `{ceiling, upgrade_trigger}`;
  пустой триггер → `no_trigger=true`; debt-леджер в отчёте.
- [x] **27.4** `ralph_report` + gain-метрики (FR-K10): repeat-failure rate (доля failed),
  reuse-hit rate (ladder_rung=reuse:*), stale-оборачиваемость, LOC/tokens на задачу.
- [x] **27.5** `ast_diff(project_id, from, to?)`, `ast_history(project_id, symbol)` —
  symbol-level дифф и хронология по итерациям.
- [x] **27.6** Тесты: reuse-кандидат (известный символ fixture попадает в пакет); stale-разметка
  при повторном изменении символа; deferred без триггера → no_trigger; бюджет пакета.

### Фаза 28 — Ralph-окружение (Оценка: 1.5–2 дня)

- [ ] **28.1** Dream-фаза 3 (FR-K7, за autodream-гейтами): ревизия stale-findings по текущему
  AST/спеке → verified (source=dream) или подтверждение stale/decay; `OB2H_DREAM_RALPH=false`.
- [ ] **28.2** Скилл `ralph-loop` (FR-K8): черновик — Приложение C спеки; деплой
  `ob2h skill install` во все harness; правила вердиктов (red tests ≠ готово; лестница минимальности).
- [ ] **28.3** doctor (диагностика M6, сироты-apply-runs), `omnes_backup` (новые таблицы
  автоматически), `omnes_stats` (блок ralph: runs/iterations/findings by verdict),
  CLI `ob2h ralph findings-to-memory` (FR-K12).
- [ ] **28.4** `ralph_*`/`ast_changes` вне синк-бандлов (ADR-K6); SYNC.md дополнить.
- [ ] **28.5** Benchmark-пакет (FR-K10, §8 спеки): fixture-репо, методика «с ob2h-знаниями / без»,
  сырые цифры в `benchmarks/`. Домен не считается готовым без измерения.
- [ ] **28.6** Приёмка спеки §11 (полный чек-лист) + кириллический путь проекта (NFR-K4).

### Фаза 29 — (Опционально) Personalized PageRank для графа знаний (Оценка: 1–2 дня)

Идея HippoRAG/2 (§8): single-step multi-hop без графовых СУБД, pure Rust поверх `graph_edges`.

- [x] **29.1** `src/graph/pagerank.rs`: итеративный PPR (damping d∈0.5–0.85 — подбирать на bench,
  старт 0.85; ≤20 итераций, L1-норма), персонализация по seed-узлам подграфа проекта;
  без новых зависимостей.
  *(Реализовано в ветке `feat/v1.4` (Ф33 PLANA_v1.4): движок generic — на вход плоские
  рёбра `(from, to, weight)`, значит одна реализация обслуживает и граф знаний, и память;
  degree-normalization + возврат массы висячих узлов в персонализацию; веса по типу ребра
  (`parse_ppr_weights`) и `normalize_entity` — рядом. Тесты: 9 юнит-тестов (цепочка
  A→B→C, hub-защита на 120 рёбрах, клампы damping/итераций, сохранение массы, битые рёбра,
  фиксация дефолтных весов).)*
- [ ] **29.2** `graph_search` mode=ppr: **dual-seed** — топ FTS/векторных матчей + phrase/entity-узлы
  (из OneKE-lite) → PPR-ранжирование узлов (вместо 1-hop-расширения); веса рёбер по типу
  (edge-type-aware, идеи HippoRAG 2 / GAAMA — §8).
- [ ] **29.3** `graph_reason`: факт-блок пополняется PPR-подграфом (пути между used_entities).
- [ ] **29.4** Тесты: синтетический multi-hop A→B→C (запрос по A достигает C, 1-hop не достигает);
  сходимость PPR; bench на живой БД.

---

## 5. Итоговый контракт инструментов MCP (33 инструмента)

Существующие 25 инструментов сохраняют 100% совместимость сигнатур. Изменения аддитивные:

1–25. — без изменений (см. PLAN_v1.2 §4);
26. **`memory_feedback(key, verdict, note?)`** — НОВЫЙ: коррекция trust по вердикту агента.
    - `memory_context(query?, max_tokens?, max_chars?, project_id?)` — добавлен опциональный `max_chars`;
    - `memory_search(query, limit?, mode?, related?, project_id?)` — добавлен опциональный `related`;
    - `graph_search(query, limit?, project_id?, provenance?, mode?)` — режим `mode=ppr` (Фаза 29, опционально).
27. **`ralph_start(project_id, feature_slug, goal, autonomy?, limits?, budget_tokens?)`** — НОВЫЙ (Фаза 26)
28. **`ralph_iteration(run_id, task_id, n, hypothesis, plan, result, tests_summary, …)`** — НОВЫЙ (авто-вердикт, AST-дельта)
29. **`ralph_verdict(iteration_id|finding_id, verdict, verdict_source, note?)`** — НОВЫЙ
30. **`ralph_context(run_id, task_id, max_tokens?, mode?)`** — НОВЫЙ (контекст-пакет с reuse-кандидатами)
31. **`ralph_report(run_id?)`** — НОВЫЙ (прогресс, debt-леджер, gain-метрики)
32. **`ast_diff(project_id, from, to?)`** — НОВЫЙ (symbol-level дифф)
33. **`ast_history(project_id, symbol)`** — НОВЫЙ (хронология символа)

Аргументы ralph_* — спека §5 (контракт v1 стабилен для режимов H и O).
Плагин Mode A экспонирует ralph_* наравне с остальными (NFR-K3 спеки).

Новые CLI-подкоманды: `ob2h bench`, `ob2h db quantize-embeddings`,
`ob2h backup --scope/--verify`, `ob2h ralph findings-to-memory`,
`sync status` + `conflicts_overwritten`.
Изменения контракта — с записью в `CHANGELOG.md` (§6 AGENTS.md).

---

## 6. Отклонённые альтернативы

- **Разделение `ob2h.db` на memory.db + graph.db** — риск миграции выше пользы после
  int8-квантования (БД ~300 МБ); пересмотреть, если БД превысит 2 ГБ.
- **sqlite-vec** — ADR-2, бэклог.
- **Автоудаление записей с низким trust** — против правила №1 («почини» = модифицируй,
  ничего не удалять без явного разрешения): только флаг-кандидат и явный `memory_forget`.
- **Реранкер по умолчанию** — латентность на каждом поиске; только за флагом и после bench.
- **Отдельное поле trust вместо переиспользования importance** — принято: смешивать
  машинный сигнал доверия с авторским весом нельзя (агент затрёт ручные приоритеты).
- **Порт Zep/Graphiti** (temporal knowledge graph) — нужен Neo4j/FalkorDB (нарушение ADR-6);
  идеи bi-temporal и edge invalidation уже покрыты trust-ревизией дрима (23.2) и
  staleness-pass'ом Ralph (27.2). Источники — §8.
- **HyDE / LLM-перефраз запроса на горячем пути** — латентность и токены на каждый ход
  prefetch; допустимо только внутри тяжёлого `graph_reason`.
- **Смена эмбеддера на EmbeddingGemma-300m сейчас** — требует ort/fastembed-rust
  (тяжёлая зависимость) и пере-embed всей БД; бэклог, решение отдельно.

---

## 7. Definition of Done (Критерии завершения v1.3)

- [ ] `cargo test` ≥ 60 тестов зелёные, `cargo clippy --all-targets` без ошибок,
      `python -m unittest discover -s plugin/tests` зелёный.
- [ ] **Bench:** recall@5 и MRR гибридного `build_context` ≥ baseline (docs/bench_baseline.md);
      p95 latency `memory_context` < 150 мс на живой БД.
- [ ] **Prefetch:** блок ≤ 8000 символов; неделя работы без spill'ов в `hook_outputs`.
- [ ] **База:** после `quantize-embeddings` файл ≤ 350 МБ (было 939 МБ); bench до/после — без деградации.
- [ ] **Бэкапы:** `backup verify` зелёный для full и quick; quick-бэкап < 50 МБ; ротация не растёт по диску.
- [ ] **Trust:** dream-отчёты содержат вердикты ревизии; ни одна запись не удалена автоматически;
      `candidate_for_forget` виден в `dream_status`.
- [ ] **Ralph:** приёмка спеки §11 закрыта (включая мини-цикл по скиллу `ralph-loop` в Hermes
      и цифры benchmark в `benchmarks/`); миграция M6 проходит на копии живой БД и на чистой;
      ручная колонка `graph_nodes.confidence` не тронута (тест схемы); кириллический путь проекта
      не ломает рескан (NFR-K4).
- [ ] `omnes_stats` показывает `embedding_backend`; `ob2h doctor` детектит fake-режим красным.
- [ ] `CHANGELOG.md` обновлён (memory_feedback, max_chars, related, backup --scope/--verify,
      ralph_*); README/ARCHITECTURE/HERMES_INTEGRATION/SYNC.md актуализированы.

---

## 8. Исследование технологий (интернет, 2026-09-13)

Что взято в план, что в бэклог, что отклонено (Ссылки проверены поиском; вердикты — с учётом
границ ADR-1…ADR-8: SQLite-only, без torch, сеть только LLM/embedding API):

| Технология / проект | Источник | Вердикт |
|---|---|---|
| **Zep / Graphiti** — temporal knowledge graph, bi-temporal рёбра, edge invalidation; лидер LongMemEval (63.8% vs Mem0 49.0%) | [arXiv 2501.13956](https://arxiv.org/html/2501.13956v1), [github.com/getzep/graphiti](https://github.com/getzep/graphiti), [сравнение фреймворков 2026](https://particula.tech/blog/agent-memory-frameworks-tested-mem0-zep-letta-cognee-2026) | Порт **нет** (требует Neo4j/FalkorDB — нарушение ADR-6). Идеи **взяты**: «знание протухает и инвалидируется» → trust-ревизия дрима (23.2) + staleness-pass Ralph (27.2) |
| **Mem0** — пайплайн фактов: LLM решает ADD / UPDATE / DELETE / NOOP при сохранении | [arXiv 2504.19413](https://arxiv.org/html/2504.19413v1), [github.com/mem0ai/mem0](https://github.com/mem0ai/mem0), [docs.mem0.ai](https://docs.mem0.ai/core-concepts/how-it-works) | **Кандидат в бэклог**: LLM-merge при `memory_save` (косинус-близкие записи → один вызов `llm_client` → UPDATE вместо дубля). Сейчас рост памяти ограничивает только trust-петля |
| **A-MEM** — Zettelkasten-заметки: link generation + memory evolution при добавлении | [arXiv 2502.12110](https://arxiv.org/abs/2502.12110), [github.com/agiresearch/A-mem](https://github.com/agiresearch/A-mem), NeurIPS 2025 | Подтверждает **23.5** (автосвязи `memory_links`) и **23.2** (эволюция памяти дримом) — тот же паттерн поверх наших таблиц |
| **HippoRAG / HippoRAG 2** — PPR по KG, single-step multi-hop; v2: dual-node (passage+phrase), LLM-фильтр триплетов, +7 F1 на ассоциативных задачах (ICML'25) | [github.com/osu-nlp-group/hipporag](https://github.com/osu-nlp-group/hipporag), [arXiv 2405.14831](https://arxiv.org/html/2405.14831v1) | **Фаза 29** (опционально): PPR pure-Rust; из v2 — dual-seed, из GAAMA — edge-type-aware веса (строки ниже) |
| **GAAMA** — hierarchical KG (4 типа узлов / 5 типов рёбер), concept-mediated, edge-type-aware PPR; LoCoMo-10 78.9% | [arXiv 2603.27910](https://arxiv.org/html/2603.27910v1), [github.com/swarna-kpaul/gaama](https://github.com/swarna-kpaul/gaama) | Идеи для Фазы 29: edge-type-aware PPR и concept-узлы как «сквозные» пути — кандидат в `memory_links`/PPR-seed |
| **OMEGA (omega-memory)** — local-first MCP-память: SQLite + sqlite-vec + ONNX, decay/compaction, contradiction detection, graph edges (архитектурный twin OB2H) | [github.com/omega-memory/omega-memory](https://github.com/omega-memory/omega-memory), [сравнение с Mem0/Zep](https://omegamax.co/blog/omega-vs-mem0-vs-zep) | **Бэклог/исследование**: compaction (Jaccard-кластеризация → summary-nodes) и contradiction-check при save — кандидаты в Фазу 23. Цифры LongMemEval (~95.4%) — self-reported, не воспроизводимы |
| **mcp-memory-service** — SQLite-vec + local ONNX, typed KG (`causes`/`fixes`/`contradicts`), scheduled consolidation (decay + кластеризация + belief derivation) | [github.com/doobidoo/mcp-memory-service](https://github.com/doobidoo/mcp-memory-service) | Подтверждает 23.2/23.5; typed edges — кандидат в расширение `memory_links.kind` (23.5), belief-derivation — в бэклог |
| **GraphQLite / sqlite-graphrag** — Cypher + алгоритмы (PageRank/Louvain) в SQLite; Rust-бинарник FTS5+cosine+multi-hop | по данным ревью владельца (проверка ссылок перед реализацией) | Идеи multi-hop expansion и symbol-history для Фазы 29; Cypher и расширения не берём — только pure-Rust алгоритмы |
| **LongMemEval** — бенчмарк 5 способностей памяти: извлечение, многосессионный синтез, knowledge update, temporal reasoning, abstention | [arXiv 2410.10813](https://arxiv.org/abs/2410.10813), [github.com/xiaowu0162/longmemeval](https://github.com/xiaowu0162/longmemeval) | **Фаза 21**: методология категорий golden set (особенно knowledge update и abstention — «правильно промолчать», если факта нет); сам бенчмарк не запускаем — персональный масштаб |
| **LoCoMo-контроверза** — Zep опубликовал rebuttal Mem0; Letta Filesystem набрала 74% на LoCoMo, просто храня транскрипты файлами | [разбор систем памяти 2026](https://blog.devgenius.io/ai-agent-memory-systems-in-2026-mem0-zep-hindsight-memvid-and-everything-in-between-compared-96e35b818da8), [getzep.com/platform/graphiti](https://www.getzep.com/platform/graphiti/) | Публичные цифры противоречивы → свой bench (Фаза 21) важнее чужих бенчмарков; файловый workspace + dream валидированы как конкурентоспособный подход |
| **sqlite-vec** — int8/binary quantization, vec_quantize_binary, **rescore ANN** (oversample + full-precision re-rank, DiskANN); pre-v1 | [github.com/asg017/sqlite-vec](https://github.com/asg017/sqlite-vec), [гайд binary quantization](https://alexgarcia.xyz/sqlite-vec/guides/binary-quant.html) | Бэклог с **повышенным приоритетом**: после фаз 21 (latency-baseline) и 24 (свой int8) — явный эксперимент «vec0 + rescore vs собственный brute-force». Кандидат в v1.4, если p95 `memory_search` > 80–100 мс на ~600K векторов |
| **fastembed rerankers** — `TextCrossEncoder`, bge-reranker-base (ONNX, CPU) | [docs Qdrant rerankers](https://qdrant.tech/documentation/fastembed/fastembed-rerankers/), [BAAI/bge-reranker-base](https://huggingface.co/BAAI/bge-reranker-base), [crates.io/fastembed](https://crates.io/crates/fastembed) | **25.3** как записано (за флагом, после bench). Альтернатива без ort-зависимости: LLM-реранк топ-20 через `llm_client` — бэклог |
| **EmbeddingGemma-300m** — мультиязычный on-device эмбеддер (ONNX q8/q4) | [onnx-community/embeddinggemma-300m-ONNX](https://huggingface.co/onnx-community/embeddinggemma-300m-ONNX), [fastembed-rust enum](https://docs.rs/fastembed/latest/fastembed/enum.EmbeddingModel.html) | **Бэклог**: смена эмбеддера = ort/fastembed-rust + пере-embed всей БД; reconsider при следующем апгрейде модели |
| **MCP spec 2026-07-28** — stateless core, Extensions framework; elicitation/sampling (с 2025-06-18) | [spec](https://modelcontextprotocol.io/specification/2026-07-28), [changelog](https://modelcontextprotocol.io/specification/draft/changelog), [блог MCP](https://blog.modelcontextprotocol.io/posts/2026-07-28/) | Локальный stdio-сервер не затронут (stateless-переход касается удалённых серверов). **Бэклог**: tool annotations (`readOnlyHint` и др. — дёшево, помогает harness'ам), elicitation для вопросов дрима |
| **Litestream** — непрерывная WAL-репликация SQLite в S3/файл | [litestream.io](https://litestream.io) | **Бэклог** для VPS-бэкапов (ADR-9 остаётся основным); вариант, если захочется автоматизации поверх `backup verify` (24.4) |
| **HyDE / query-expansion** — LLM-перефраз запроса перед embed | общая практика RAG | **Отклонено** для горячего пути prefetch (латентность+токены на каждый ход); допустимо внутри `graph_reason` |

**Пополняем бэклог:** contradiction-check / LLM-merge при `memory_save` (Mem0 + OMEGA-style),
typed edges + belief-derivation в dream-ревизии, compaction (кластеризация → summary-nodes)
как альтернатива/дополнение candidate_for_forget, эксперимент sqlite-vec rescore после
latency-baseline (критерий v1.4: p95 > 80–100 мс), лёгкий hierarchical tiering
(working/episodic/semantic, TiMem-style) после trust, bge-reranker/LLM-реранк,
EmbeddingGemma-300m, sqlite-vec (см. строку выше), MCP-аннотации/elicitation, Litestream,
Layer3-профиль пользователя, GBAM-зеркало сессий (из PLAN.md).
