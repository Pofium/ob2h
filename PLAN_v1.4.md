# 🚀 План разработки OB2H v1.4: Ночной гейт качества, консолидация памяти, sync v2

> **Версия плана:** 1.4.0
> **Статус:** Черновик rev.2 — правки по ревью (2026-09-13); на утверждение
> **Обновлён:** 2026-09-13 — rev.2 по ревью: dual-сигнал гейта Ф30 (recall@5 или MRR), timeout ≠ rollback, supersedes в вердиктах LLM (Ф31), лимиты compaction, обе стороны в conflict-разметке (Ф32), soft-delete рёбер (миграция M7), meta.conflict_versions в синке (Ф34), §8 исследование ревью (MELD/Hindsight/Governed Memory/StateFuse); явное предусловие — закрытый DoD v1.3
> **Предыдущие этапы:** v0.8/0.9 (Ядро, Память, Дриминг, Синк), v1.0/1.1 (AST-граф, God Nodes, мультиагентность), v1.2 (Zero-Config проекты, инкрементальный AST, AutoSync, семантика кода), v1.3 rev.2 (bench, честный prefetch, trust-петля, квантование/бэкапы, Ralph Knowledge Layer, опциональный PPR)
> **Принцип совместимости:** 100% обратная совместимость (Zero Breaking Changes) для всех 33 инструментов MCP. Изменения аддитивные (§5), с записью в `CHANGELOG.md`.
> **База:** кандидаты взяты из §8/бэклога утверждённого PLAN_v1.3 (rev.2, коммит 5a3e32f) — sqlite-vec-rescore, typed edges, compaction/contradiction-check, LLM-merge; нумерация фаз продолжается после Фазы 29 (Ralph-трек + PPR).
> **Предусловие старта:** v1.4 не стартовать до закрытия DoD v1.3 (Фазы 21–25, включая 23.5 и 24) — иначе ночной гейт будет мерить «старый» importance-only `build_context`, а Ф31–33 зависят от M5 (trust, memory_links). На main сейчас v1.2.0 — план валиден как post-v1.3.

---

## 1. Мотивация (что осталось за рамками v1.3)

Baseline Фазы 21 (коммит a2b473e, живая БД): search recall@5 = 0.222 / MRR = 0.198,
context recall@5 = 0.833 / MRR = 0.861 — у вынесенного в явный поиск запас качества большой.
Аргумент Ф30: вынос гибрида в `build_context` (v1.3) дал скачок — теперь нельзя дать дриму
это съесть без отката.

| Проблема | Факт | Чем закрываем |
|---|---|---|
| Дрим не проверяет себя по метрикам | 54 dream-коммита правят MEMORY/SOUL/USER; деградация retrieval после дрима обнаруживается вручную (или никогда) | **Ф30**: ночной bench-гейт, авто-rollback через `dream_restore` |
| Записи конкурируют вместо слияния | почти-дубли не схлопываются; противоречащие правила обе попадают в контекст; рост памяти ограничен только trust-петлёй (§8 v1.3, строка Mem0) | **Ф31**: LLM-merge офлайн, contradiction-check при save, compaction → summary-nodes, `memory_merge` |
| Вердикты дрима не становятся структурой | 23.2 даёт verdict строкой; зарезервированные kind `contradicts\|causes\|supersedes` (23.5) пока никто не заполняет | **Ф32**: typed edges в dream-ревизии, conflict-разметка в выдаче |
| PPR есть только у документного графа | Ф29 v1.3 — PPR по `graph_edges`; `memory_links` остаются 1-hop (23.5) | **Ф33**: PPR по памяти — dual-seed, edge-type-aware (HippoRAG 2 / GAAMA, §8 v1.3) |
| Синк обменивается целиком, конфликты — запись целиком | 25.4 дал только счётчик `conflicts_overwritten`; `memory_links`, `trust`, feedback-журнал в v1-бандлы не входят | **Ф34**: дельта-бандлы по курсору, поле-уровневый merge, `sync verify` |
| Латентность векторов не измерена после квантования | критерий владельца (§8 v1.3, строка sqlite-vec): эксперимент «vec0 + rescore vs brute force», если p95 `memory_search` > 80–100 мс на ~600K векторов | **Ф35**: замер → гейт-эксперимент → ADR-решение; попутно — решение по реранкеру, MCP-аннотации, ретеншн логов |

**Порядок:** Ф30 сразу (глоток безопасности) → Ф31/Ф32 (консолидация) → Ф33 → Ф34/Ф35 параллельно.
**Максимальный ROI:** Ф30 → Ф31.1/31.2 → Ф34; Ф33 и Ф35 можно параллелить после.

---

## 2. Архитектурный обзор v1.4

```
              ┌────────────────────────────────────────────┐
              │  AutoDream (v1.2/1.3): дрим → commit       │
              └─────────────────┬──────────────────────────┘
                                │
              ┌─────────────────▼──────────────────────────┐
              │  НОЧНОЙ BENCH-ГЕЙТ (Ф30, новое)            │
              │  bench quick-set → сравнение с bench:last  │
              │  деградация recall@5>10% ИЛИ MRR>15%       │
              │    → dream_restore + алерт                 │
              │  timeout → warning, БЕЗ rollback           │
              │  история: data/bench/history.jsonl         │
              └─────────────────┬──────────────────────────┘
                                │
   ┌────────────────────────────▼─────────────────────────────────┐
   │  КОНСОЛИДАЦИЯ (Ф31–32, новое)                                │
   │  save: дешёвый cosine → identity-дубль = тихий UPDATE,       │
   │  подозрение = merge_candidate (LLM — офлайн, в дриме)        │
   │  dream: вердикт merge|keep_both|contradicts|supersedes;      │
   │  compaction → summary-nodes (≤8, не high-trust);             │
   │  verdict → typed edges (contradicts/supersedes/causes);      │
   │  conflict-разметка: обе стороны + trust                      │
   └────────────────────────────┬─────────────────────────────────┘
                                │
   ┌────────────────────────────▼─────────────────────────────────┐
   │  SQLite (data/ob2h.db)                                       │
   │  memory_links + typed edges ──► PPR по памяти (Ф33)          │
   │  M7: memory_links.deleted_at (soft-delete рёбер)             │
   │  vec0-эксперимент за флагом (Ф35; ADR-решение по итогам)     │
   └────────────────────────────┬─────────────────────────────────┘
                                │
              ┌─────────────────▼──────────────────────────┐
              │  SYNC v2 (Ф34): дельта по курсору,         │
              │  поле-уровневый merge БЕЗ молчаливого LWW: │
              │  conflicts.jsonl + meta.conflict_versions, │
              │  trust+links+feedback в бандле, verify     │
              └────────────────────────────────────────────┘
```

---

## 3. Модель данных и файлы

**Одна аддитивная миграция M7:** `ALTER TABLE memory_links ADD COLUMN deleted_at TEXT;`
(soft-delete рёбер при forget стороны — чтобы sync v2 реплицировал удаление, как tombstones
записей). Остальное — kv и файлы; typed kind уже зарезервированы в 23.5.

- kv: `bench:baseline`, `bench:last`, `bench:runs` (счётчик прогонов для гейта Ф35.2),
  `sync_cursor:<peer>` (max `updated_at` отправленного), `vec:index_stats` (для Ф35.1).
- `data/bench/history.jsonl` — `{ts, mode, recall@5, recall@10, mrr, p95_ms, db_size_mb,
  embedding_backend, dream_sha}` — бэкенд и размер БД обязательны: ночные регрессии
  от подмены модели/Fake видны в тренде.
- `data/sync/conflicts.jsonl` — поле-уровневые конфликты (дополняет счётчик 25.4).
- Формат синк-бандла v2 (миграция формата, не схемы): `manifest{format:2, since, counts}` +
  изменённые строки `memories` (с trust, last_feedback_at, meta.feedback), `memory_links`
  (включая soft-delete рёбер M7), `kv`, session-логи. `ralph_*`/`ast_changes` по-прежнему
  вне бандлов (ADR-K6). Читатель v1.4 понимает v1 и v2; старый бинарник на v2 — внятная
  ошибка «format:2».

---

## 4. Детальные фазы реализации

### Фаза 30 — Ночной bench-гейт дрима (Оценка: 1.5 дня)

Цель: дрим, ухудшивший retrieval, откатывается той же ночью — замыкание петли обучения.

- [ ] **30.1** В `AutoDreamWorker` после успешного дрима: прогон quick-набора bench
  (15 запросов golden set, mode=context, бюджет `OB2H_BENCH_GATE_TIMEOUT_MS`, дефолт 3000).
- [ ] **30.2** Откат при деградации **любого** из двух сигналов: recall@5 > 10% или
  MRR > 15% относительно `bench:last` → `dream_restore` на предыдущий workspace-коммит,
  `bench:last` не обновляется, в дрим-отчёт — алерт «ОТКАТ: дрим <sha> ухудшил
  recall@5 с X до Y / MRR с A до B» (падение только MRR при стабильном recall — тоже
  деградация). Иначе — обновление `bench:last` + строка в `history.jsonl`.
  **Timeout ≠ rollback:** не уложился в бюджет — warning, гейт пропущен, `bench:last`
  не трогается (никаких решений по неполным данным).
- [ ] **30.3** Гейты: golden set существует; `OB2H_BENCH_GATE=1` (дефолт off до набора
  статистики).
- [ ] **30.4** `ob2h bench history [--last N]` — тренд по `history.jsonl`; в `dream_status` —
  исход последнего гейта.
- [ ] **Тесты:** FakeLLM-дрим, реально портящий MEMORY.md (не только trust) → rollback,
  workspace на предыдущем sha; деградация только MRR (recall стабилен) → тоже rollback;
  timeout-путь → warning, БЕЗ rollback; здоровый дрим → baseline обновлён;
  без golden set — гейт пропущен с warning.

### Фаза 31 — Консолидация при записи и дедуп (Оценка: 2–2.5 дня)

Цель: дубли схлопываются, противоречия фиксируются в момент записи; LLM — только офлайн.

- [ ] **31.1** Дешёвый save-time пре-чек (`memory_save`, без LLM): топ-1 косинусный сосед.
  cos ≥ 0.98 (identity-дубль, «одно и то же») → тихий UPDATE (union `meta`, max importance,
  +1 access_count); cos 0.75–0.95 → пометка `meta.merge_candidate=<key>` — кандидат в
  дрим-ревизию. Пороги согласованы с практикой Governed Memory (write 0.92 / background
  0.95 — §8); identity-дубль отличаем от state-update (см. 31.2).
- [ ] **31.2** LLM-merge офлайн (бэклог §8 v1.3, строка Mem0): в дриме для групп
  `merge_candidate` один вызов `llm_client` с вердиктом
  `merge | keep_both | contradicts | supersedes` (исходы MELD — §8):
  identity-дубль → `merge` (UPDATE канонической записи, остальные — tombstone +
  `meta.merged_into`); state-update («раньше Postgres, теперь MySQL») → `supersedes` —
  обе записи живы, ребро kind=supersedes из Ф32 (история не теряется);
  на горячем пути save LLM нет.
- [ ] **31.3** Compaction (OMEGA-style, §8 v1.3): в дриме раз в 30 дней кластеризация
  (Jaccard по ключам + косинус) записей с низким trust/давним доступом → LLM пишет
  summary-node `hmem-digest/<cluster>`, связанную kind=summary с членами. Ограничения:
  кластер ≤ 8 записей (иначе дайджест теряет конкретику); high-trust (≥ 0.7) и
  high-access записи в кластеры не попадают. Оригиналы не трогаются (дополнение к
  `candidate_for_forget`, не замена).
- [ ] **31.4** MCP-инструмент **`memory_merge` (№34)**: `memory_merge(keys[], canonical_key?, note?)`
  — явное подтверждённое слияние (то же, что решает LLM, но рукой агента); перед
  применением дрим печатает dry-run план в отчёте (как `ob2h memory dedup`).
  Авто-слияние запрещено (правило №1): только инструмент или дрим-вердикт с записью
  в отчёте.
- [ ] **31.5** CLI `ob2h memory dedup [--dry-run]` — отчёт: группы-кандидаты, предлагаемый
  канонический ключ, план слияния.
- [ ] **Тесты:** детерминированный пре-чек на фиксированных векторах (0.98/0.95/0.75
  границы); merge сохраняет max importance, sum access, перенос links; tombstone вне
  search; вердикт `supersedes` — обе записи живы + ребро; high-trust кластер не сжимается;
  дайджест не заменяет оригиналы; FakeLLM-вердикты `keep_both` ничего не меняют.

### Фаза 32 — Typed edges в dream-ревизии (Оценка: 1.5–2 дня)

Цель: вердикты 23.2 становятся рёбрами; противоречащие записи перестают обе выдаваться молча.

- [ ] **32.1** Вердикт `contradicted` → ребро kind=`contradicts` между старой и новой записью;
  `outdated` → kind=`supersedes` (новая supersede старую); `confirmed` → только trust-bump
  (как 23.2). Резерв 23.5 начинает работать, API kind не меняется.
- [ ] **32.2** Conflict-разметка в выдаче: если среди хитов есть пара, связанная
  `contradicts`, — под блоком показываются **обе** записи с их trust:
  `[conflict] key_a (trust 0.8) ↔ key_b (trust 0.3): вердикт дрима <ts>` — агент видит
  спор целиком, а не только «победителя».
- [ ] **32.3** Belief-derivation lite (бэклог §8 v1.3, mcp-memory-service): в дриме LLM может
  предлагать kind=`causes` между записями; флаг `OB2H_DREAM_BELIEF=false` (дефолт off);
  предложения — только в дрим-отчёт до включения флага.
- [ ] **32.4** Forget стороны ребра → soft-delete ребра (M7: `deleted_at`), не жёсткое
  удаление — удаление реплицируется синком v2, как tombstones записей.
- [ ] **32.5** `memory_links` входят в синк-бандл v2 (Ф34) — typed edges реплицируются.
- [ ] **Тесты:** вердикты FakeLLM → ожидаемые рёбра; повторный дрим не дублирует рёбра;
  forget стороны → ребро soft-deleted, sync v2 возит; conflict-разметка показывает обе
  стороны с trust; флаг off — рёбра `causes` не создаются.

### Фаза 33 — PPR по памяти: multi-hop reasoning на `memory_links` (Оценка: 1.5–2 дня)

Цель: «что я знаю про X и с чем это связано» — по цепочке памяти с уверенностью.

- [ ] **33.1** Переиспользование `src/graph/pagerank.rs` (Ф29 v1.3): память как отдельный
  граф (`memory_links`), **dual-seed** — гибридные хиты + entity-фразы записей (OneKE-lite),
  **edge-type-aware** веса в конфиге `OB2H_PPR_WEIGHTS` с дефолтами, зафиксированными
  тестом: manual=1.0, contradicts/causes=0.8, category=0.5, same_project=0.3.
  Hub-protection обязательно: damping + degree-normalization — иначе PPR залипает
  на записи-хабе.
- [ ] **33.2** `graph_reason(query, project_id?, scope?)` — опциональный
  `scope: docs|memory|all` (дефолт `all` = совместимость): memory-режим — PPR-подграф
  по памяти, уверенность = произведение trust вершин на вес пути, лимит 500 узлов / 1 с.
- [ ] **33.3** `memory_search` mode=graph: PPR-расширение вместо 1-hop (23.5) при
  `related=true`; fallback на 1-hop, если PPR недоступен/не построен.
- [ ] **33.4** Нормализация сущностей при построении рёбер: lowercase, ё→е, схлопывание
  инициалов/форм — меньше шумовых same_project/entity-рёбер.
- [ ] **Тесты:** синтетическая цепочка A→B→C по memory_links — запрос по A достигает C
  (1-hop не достигает); hub-защита: запись со 100+ рёбрами не «залипает» выдачу
  (degree-normalization); дефолтные веса зафиксированы тестом; вызовы без `scope` —
  выдача не изменилась (регресс-тест контракта); bench до/после на живой БД.

### Фаза 34 — Sync v2: дельты, поле-уровень, целостность (Оценка: 2–2.5 дня)

Цель: обмен PC↔VPS — только изменённое; конфликты — по полям, с журналом, без молчаливого LWW.

- [ ] **34.1** Дельта-экспорт: курсор `sync_cursor:<peer>`, бандл v2 — только изменённые/
  новые строки; `ob2h sync push --full` — полный (ежемесячно/по запросу); дефолт — дельта.
- [ ] **34.2** Поле-уровневый merge при apply: `content/importance/category/trust` — LWW по
  `updated_at`, но **не молча**: конфликт content всегда пишется в `sync/conflicts.jsonl`
  (дополняет счётчик 25.4); при флаге `OB2H_SYNC_KEEP_LOSERS=1` проигравшая версия
  сохраняется в `meta.conflict_versions` (идея StateFuse, §8) — видно, что именно
  «победил VPS». `meta` — глубокое объединение (union ключей, LWW по значению);
  `access_count` — max.
- [ ] **34.3** В v2-бандл входят `memory_links` (с M7 soft-delete), `trust`, `last_feedback_at`,
  feedback-журнал из `meta` (v1 их не возил). `ralph_*`/`ast_changes` — по-прежнему вне
  (ADR-K6).
- [ ] **34.4** `ob2h sync verify [--peer vps]` — сверка без переноса: counts (memories,
  links), trust_avg, контрольные суммы по `key`+`updated_at` с обеих сторон, отчёт о дрейфе.
- [ ] **Тесты:** дельта после N правок переносит ровно N строк; round-trip PC→VPS→PC
  с trust/links — без потерь; конфликт content при флаге — проигравшая версия в
  `meta.conflict_versions`; v1-бандл читается; verify детектит искусственный дрейф;
  ralph-таблицы в бандл не попадают.

### Фаза 35 — Латентность, выдача, гигиена (Оценка: 2–3 дня)

Цель: закрыть латентный вопрос владельца и дешёвые улучшения из бэклога §8 v1.3.

- [ ] **35.1** Замер p95 `memory_search` на живой БД после квантования (Ф24) — в
  `docs/bench_baseline.md`, отдельно для `memories` и `graph_nodes` (основной объём —
  граф). **Гейт владельца:** если p95 > 80–100 мс на ~600K векторов → эксперимент
  «sqlite-vec rescore vs собственный brute force»: vec0 virtual table, binary/int8-
  квантование + oversample + full-precision re-rank, за флагом `OB2H_VEC0=1`;
  ориентиры (§8): rescore int8 os=2 ≈ 2.6× при recall@10 ≈ 1.0. Критерий — recall@10
  ≥ 0.99 от brute force на golden set и p95 < 50 мс; итог — ADR-запись в `PLAN.md`/`docs/`
  с цифрами p95 до/после **независимо от решения** (принять sqlite-vec / остаться на
  brute force). DiskANN — бэклог: только если rescore не хватит (дорогой insert).
  Если p95 ≤ порога — эксперимент откладывается, решение фиксируется с цифрами.
- [ ] **35.2** Решение по реранкеру (25.3): после ≥ 30 ночных прогонов (30.3) — если
  rerank-профиль дал MRR +5% **и** p95 не вырос > 30% — флаг `OB2H_RERANK` переворачивается
  в дефолт `1`; иначе остаётся off, решение — в bench-отчёте с числами. LLM-реранк топ-20
  через `llm_client` — запасная альтернатива без ort (бэклог §8 v1.3).
- [ ] **35.3** MCP tool annotations (бэклог §8 v1.3): `readOnlyHint` на read-only инструменты
  (memory_search/context, graph_search/reason/stats, project_*…), `destructiveHint` на
  memory_forget — дёшево, помогает harness'ам, контракт аргументов не трогает.
- [ ] **35.4** Ретеншн workspace-логов: daily-логи старше `OB2H_LOG_RETENTION_DAYS=90`
  упаковываются в `data/workspace/archive/YYYY-MM.jsonl.gz` (архивация, не удаление).
- [ ] **Тесты:** vec0-эксперимент на синтетике 10K/1M (fixture-набор) — recall и p95
  сравнимы с brute force; флаг off — поведение байт-в-байт прежнее; аннотации видны в
  `tools/list`; архивация не трогает свежие логи; benchmark-методика Ralph (28.5) не задета.

---

## 5. Итоговый контракт инструментов MCP (34 инструмента)

Существующие 33 инструмента сохраняют 100% совместимость сигнатур. Изменения аддитивные:

1–33. — без изменений (см. PLAN_v1.3 §5);
34. **`memory_merge(keys[], canonical_key?, note?)`** — НОВЫЙ (Ф31): подтверждённое
    слияние почти-дублей; tombstone + redirect для links.
    - `graph_reason(query, project_id?, scope?)` — добавлен опциональный `scope: docs|memory|all` (Ф33);
    - `memory_search(..., mode?)` — добавлено значение `mode=graph` (Ф33, fallback 1-hop).

Новые CLI-подкоманды: `ob2h bench history`, `ob2h memory dedup`, `ob2h sync verify`,
`ob2h sync push --full`; флаги `OB2H_BENCH_GATE`, `OB2H_BENCH_GATE_TIMEOUT_MS`,
`OB2H_PPR_WEIGHTS`, `OB2H_DREAM_BELIEF`, `OB2H_SYNC_KEEP_LOSERS`, `OB2H_VEC0`,
`OB2H_RERANK` (решение по дефолту — 35.2), `OB2H_LOG_RETENTION_DAYS`.
Изменения контракта — с записью в `CHANGELOG.md` (§6 AGENTS.md).

---

## 6. Новые зависимости

- **sqlite-vec** — только как эксперимент Ф35.1 за флагом `OB2H_VEC0=1`, cargo feature
  optional; НЕ подключается в дефолтную сборку до ADR-решения по итогам эксперимента
  (ADR-2 формально остаётся «бэклог» до закрытия Ф35.1). Это единственная кандидат-зависимость.

## 7. Отклонённые альтернативы

- **usearch/HNSW-in-process** — владелец зафиксировал путь sqlite-vec-rescore (§8 v1.3);
  HNSW возвращается в рассмотрение, только если vec0-эксперимент провалится по recall.
- **Query rewrite / HyDE на горячем пути prefetch** — отклонено в v1.3 §8 (латентность
  и токены на каждый ход); не возвращаем. Допустимо только внутри тяжёлого `graph_reason`.
- **LLM-merge на горячем пути save** — латентность каждого сохранения; LLM только офлайн
  в дриме (31.2), на save — дешёвые косинусные проверки (31.1).
- **Автослияние/автоудаление дублей** — против правила №1: только dry-run-отчёт, явный
  `memory_merge`/`memory_forget`, дрим-вердикты с журналом.
- **Полный CRDT для синка (MELD/StateFuse целиком)** — overkill для 2 узлов; берём только
  идею сохранения проигравшей версии (`meta.conflict_versions`, 34.2).
- **Vector clocks / per-peer watermark** — нужно при 3+ peers; для PC↔VPS хватает курсора
  `updated_at`; бэклог.
- **Hierarchical tiering (working/episodic/semantic, TiMem-style) сейчас** — нужен
  накопленный trust-статистики (§8 v1.3: «после trust»); бэклог после v1.4.
- **Смена эмбеддера на EmbeddingGemma-300m, Litestream, MCP elicitation** — бэклог §8 v1.3,
  не фазы.
- **Веб-дашборд памяти** — требует изменения ADR «Сеть — только LLM/embedding API»;
  кандидат в бэклог, не фаза.

---

## 8. Исследование ревью (2026-09-13)

Технологии из ревью плана — что берём, что в бэклог:

| Технология | Вердикт для v1.4 |
|---|---|
| **MELD** (arXiv 2608.16357) — 5 исходов merge (insert/merge/relate/conflict/reject), contradiction first-class, status CRDT | Идеи в Ф31/Ф34: исходы ≈ `merge\|keep_both\|contradicts\|supersedes`; silent rewrite запрещён. Полный CRDT — overkill для 2 узлов |
| **Hindsight consolidation** — offline merge + conflict policies | Подтверждает Ф31 (LLM-merge офлайн, на hot path — только дешёвый cosine, без adjudication) — уже в духе плана |
| **Governed Memory** — пороги cosine ≥ 0.92 (write-dedup) / 0.95 (background) | Близки к 31.1 (0.98 / 0.75–0.95) — явная ссылка |
| **StateFuse / conflict-preserving CRDT** — сохранение проигравшей версии | `meta.conflict_versions` в Ф34.2 — за флагом, вместо pure LWW |
| **DiskANN** — до 17–100×, но дорогой insert | Бэклог: только если sqlite-vec rescore не хватит (Ф35.1) |
| **sqlite-vec rescore, 2026-цифры** — int8 os=2 ≈ 2.6× (recall@10 ≈ 1.0); bit os=8 ≈ 5.8× (recall ≈ 0.988) | Ф35.1: обосновывает гейт 80–100 мс — после int8 brute force на ~600K×384d часто ещё укладывается в 50–100 мс на CPU |

---

## 9. Definition of Done (Критерии завершения v1.4)

- [ ] `cargo test` ≥ 75 тестов зелёные, `cargo clippy --all-targets` без ошибок,
      `python -m unittest discover -s plugin/tests` зелёный.
- [ ] **Гейт (Ф30):** неделя работы — ночные прогоны в `bench history`; искусственно
      «вредный» дрим в тесте откатывается автоматически (dual-сигнал: и recall, и MRR);
      timeout-путь протестирован — warning, без rollback; без golden set — гейт пропускается.
- [ ] **Консолидация (Ф31):** dedup-отчёт на живой БД (`memory dedup --dry-run` — ненулевой
      отчёт или явный «0 candidates»); merge через `memory_merge` и через дрим-вердикт
      протестированы; ни одна запись не удалена автоматически; дайджесты появляются
      в search как entry-points.
- [ ] **Typed edges (Ф32):** conflict-разметка показывает обе стороны с trust; forget стороны
      → soft-delete ребра, реплицируемый синком; старые вызовы без новых аргументов —
      выдача не изменилась.
- [ ] **PPR-память (Ф33):** multi-hop A→B→C достигается по memory_links; hub-защита
      проверена тестом; p95 `graph_reason(scope=memory)` < 1 с на живой БД.
- [ ] **Sync (Ф34):** типичный дневной дельта-бандл < 1 МБ; round-trip PC→VPS→PC
      с trust/links — без потерь; `sync verify` детектит дрейф; v1-бандлы читаются;
      `conflicts.jsonl` журналирует поле-уровневые конфликты.
- [ ] **Латентность (Ф35):** p95 `memory_search` замерен (отдельно memories/graph_nodes)
      и записан; ADR-запись в `PLAN.md`/`docs/` с цифрами p95 до/после — независимо от
      решения по sqlite-vec; решение по реранкеру зафиксировано в bench-отчёте с числами.
- [ ] `CHANGELOG.md` обновлён (memory_merge, scope, mode=graph, sync v2, bench history,
      tool annotations); README/ARCHITECTURE/HERMES_INTEGRATION/SYNC.md актуализированы;
      при подключении sqlite-vec — запись в `PLAN.md` §6 (журнал решений).
