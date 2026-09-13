# ПРОЕКТ B: «Ralph Knowledge Layer» — расширение ob2h v1.3.0

**База знаний циклов разработки и AST-трекинг как дополнительные функции
при подключении ob2h к harness-системам: Hermes, ZCode, Claude Code, Gemini CLI,
OpenCode, Qwen Code, Cursor, Windsurf и др.**

| | |
|---|---|
| Версия | 2.0 (полностью самодостаточный документ) |
| Дата | 2026-09-09 |
| Репозиторий | `C:\Projects\omnesbot_for_hermes` (ob2h), целевая версия **v1.3.0**, миграция **M4** |
| Статус | Независимый проект. Работает БЕЗ Проекта A (омнес-оркестратора); любой harness-агент ведёт цикл сам |
| Поглощено | Концепция **Ponytail** (DietrichGebert/ponytail): reuse-ступень через кодовый индекс, debt-семантика маркеров, режимы lite/full, культура честного бенчмарка — §2 |

---

## 1. Общие сведения

### 1.1. Цель
Дополнить ob2h (память, граф знаний, проектный AST-анализ, дриминг, синк)
доменом **«циклы разработки»**: итерации агента, размышления с вердиктами
«верно / не верно», symbol-level трекинг изменений кода, сборка контекстных
пакетов для чистого рестарта цикла — включая детерминированные **кандидаты на
переиспользование** из AST-графа (лучшее из Ponytail, которого у него самого нет).

### 1.2. Границы независимости
- Проект **не требует** омнес-оркестратора: основным потребителем является сам
  LLM-агент в harness-системе (режим H, §3).
- Спецификация фичи живёт в репозитории проекта (OpenSpec-раскладка или любой
  tasks.md); ob2h хранит только знания и следы изменений, в репозиторий не пишет.
- Все новые инструменты — обычные MCP-инструменты: доступны любому агенту,
  к которому подключён ob2h.

### 1.3. Словарь
Ralph Loop, дельта-спека, итерация, размышление (finding), вердикт
(verified/failed/unconfirmed/overturned/stale), staleness, лестница Ponytail,
`ponytail:`-маркер (debt), reuse-кандидаты.

---

## 2. Анализ Ponytail: что взято и почему

| # | Идея Ponytail | Как интегрируем в ob2h |
|---|---|---|
| 1 | **Ступень 2 лестницы** («уже есть в кодовой базе — переиспользуй») сегодня проверяется агентом на глаз | **Killer-фича:** ob2h имеет детерминированный AST-граф — `ralph_context` включает блок **reuse-кандидатов** (семантический поиск по символам кода через существующий `project_graph_search`), т.е. ступень 2 становится измеримой и полной по охвату (§6) |
| 2 | **`ponytail:` маркеры** осознанных упрощений (ceiling + upgrade path) | Новый kind finding — `deferred`; леджер в `ralph_report`; маркеры без upgrade-триггера — тег `no-trigger` (§7.2) |
| 3 | **`/ponytail-gain`: честный scoreboard** (агentic-бенчмарк, git-diff, контрольные армы, safety-тир) | **Benchmark-культура:** сравнение «цикл с знаниями ob2h / без», метрика ценности памяти — repeat-failure rate и reuse-hit rate (§8) |
| 4 | **Режимы lite/full + дефолт через env/config** | Режимы в скилле `ralph-loop` (§7.1) и в `ralph_context` (lite — короче пакет, без архитектурной зоны) |
| 5 | **«Не-отсекаемое»** (валидация/data-loss/безопасность/a11y) | Фиксируется в скилле как правила вердиктов: verified не ставится за счёт carved-out зон (тесты обязаны покрывать границы) |
| 6 | **ONE runnable check** | В скилле: критерий готовности задачи — минимальная запускаемая проверка, а не тяжёлый тестовый стек, если спека не требует |

**Не берём:** terse-prose стиль, plugin/marketplace-механику 20 хостов (у нас один
транспорт — MCP + `agent install`), npm lifecycle-хуки.

---

## 3. Подключение к harness-системам

### 3.1. Транспорт — существующий, без изменений
- **Hermes:** `config.yaml → mcp_servers.ob2h` (подключён); после релиза новые инструменты появляются автоматически. MemoryProvider-плагин (Mode A) экспонирует их же.
- **ZCode, Claude Code, Gemini/Antigravity, Qwen Code, OpenCode, Cursor/Windsurf:** `ob2h agent install --agent <id>|all` обновляет конфиги; `ob2h doctor --fix` диагностирует.
- Никаких новых демонов/портов: MCP stdio + долгоживущий процесс (ADR-K1).

### 3.2. Два режима потребления
| Режим | Кто пишет | Сценарий |
|---|---|---|
| **H — Harness-native** (основной) | LLM-агент в диалоге | Агент Hermes/ZCode/Claude Code ведёт цикл сам, вызывая `ralph_*` инструменты; процедура — в скилле `ralph-loop` |
| **O — Orchestrated** (опционально) | любой оркестратор поверх публичного MCP-контракта; для omnes-agent — его **ob2h-bridge** (скилл + тулзы `ob2h_bridge_*` + конфиг `[ob2h_bridge]`), точечно по проектам | Цикл может дублировать findings/summary в ob2h; низовой интеграции (общая БД, прямой доступ) нет — только публичный контракт |

Ключевое отличие от «сырого» Ralph-цикла: в режиме H **вердикты ставит ob2h по
объективным сигналам** (exit-code тестов), самооценка модели не влияет — модель
не может объявить «готово» на красных тестах.

### 3.3. Скилл `ralph-loop` (обязательная поставка)
`ob2h skill install` ставит скилл во все поддержанные harness (механизм существующий).
Содержание: лестница Ponytail (канонический блок Проекта A, Приложение C),
процедура цикла (§7.1), правила вердиктов, режимы lite/full, debt-леджер.
Черновик SKILL.md — Приложение C.

### 3.4. Обмен с семейством omnes (конвенции моста)
omnes-agent (Проект A) подключает ob2h только через свой **ob2h-bridge** —
скилл + тулзы `ob2h_bridge_{status,push,pull}` + конфиг `[ob2h_bridge]`.
**Низовой интеграции нет** (никаких общих БД и прямого доступа к SQLite) —
только публичные интерфейсы. Со стороны ob2h мост работает на существующих
инструментах без изменений схемы M4:
- namespace ключей импортируемых записей: `omnesagent:<project_id>:…`,
  `source='bridge:omnesagent'`; записи с чужим префиксом **не реэкспортируются**
  (эхо-защита от циклического дублирования между системами);
- обмен — нейтральный envelope v1 `{schema_version, origin, project_id, kind,
  key, content, verdict, symbols[], meta, updated_at}`; маппинг дивергентных
  колонок (omnesagent `agent_id/namespace/session_id` ↔ ob2h
  `importance/source/origin/deleted_at`) выполняет мост;
- по умолчанию импортируется только `verified`-контент и summary
  (fail-знания остаются локальными у источника);
- конфликт key → LWW по `updated_at`; удаление — tombstone, не молчаливое
  стирание; дедуп по content-hash, идемпотентный push/pull;
- обмен инициируется явно (человек/скилл: push после Archive, pull перед новой
  фичей того же проекта); фоновых автосинков нет. ob2h при выключенном мосте
  полностью автономен.

---

## 4. Модель данных (миграция M4)

### 4.1. Ограничения совместимости (жёсткие)
1. Миграция только аддитивная; существующие таблицы структурно не меняются.
2. **Не пересоздавать `graph_nodes`** — в живой БД вручную добавлена колонка `confidence`, отсутствующая в схеме v4.
3. Перед применением — бэкап `backups/pre-m4-*.db`; версия схемы → **v5**.

### 4.2. DDL
```sql
-- M4: Ralph — циклы, итерации, размышления, AST-дельты

CREATE TABLE ralph_runs (
  id TEXT PRIMARY KEY,                    -- ULID
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  feature_slug TEXT NOT NULL,             -- openspec/changes/<slug>
  goal TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'proposed',
    -- proposed|applying|verifying|archived|stopped|failed
  autonomy TEXT NOT NULL DEFAULT 'L1',    -- информационно; гейты исполняет писатель
  max_iterations_per_task INTEGER NOT NULL DEFAULT 5,
  max_total_iterations INTEGER NOT NULL DEFAULT 60,
  budget_tokens INTEGER,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  finished_at TEXT, stop_reason TEXT,     -- success|plateau|limit|budget|human
  UNIQUE (project_id, feature_slug)
);

CREATE TABLE ralph_iterations (
  id TEXT PRIMARY KEY,                    -- ULID
  run_id TEXT NOT NULL REFERENCES ralph_runs(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL,                  -- T-003 из tasks.md
  n INTEGER NOT NULL,                     -- номер итерации в рамках задачи
  git_before TEXT, git_after TEXT,
  hypothesis TEXT,                        -- размышление: почему не работало / как решаем
  plan TEXT,                              -- JSON: шаги
  result TEXT,                            -- JSON: итог, ошибки, self_assessment
  tests_summary TEXT,                     -- JSON: {passed, failed, fingerprint}
  verdict TEXT NOT NULL DEFAULT 'unconfirmed'
    CHECK (verdict IN ('verified','failed','unconfirmed','overturned','stale')),
  verdict_source TEXT,                    -- auto_tests|auto_verify|human|dream
  ladder_rung TEXT,                       -- Ponytail: ступень решения (reuse|stdlib|platform|dep|one-line|minimal)
  context_ref TEXT,                       -- хэш/путь контекстного пакета
  tokens_used INTEGER, duration_ms INTEGER,
  created_at TEXT NOT NULL,
  UNIQUE (run_id, task_id, n)             -- идемпотентность
);

CREATE TABLE ralph_findings (             -- атомарные размышления (переиспользуемое знание)
  id TEXT PRIMARY KEY,                    -- ULID
  run_id TEXT REFERENCES ralph_runs(id) ON DELETE SET NULL,
  iteration_id TEXT REFERENCES ralph_iterations(id) ON DELETE SET NULL,
  project_id TEXT REFERENCES projects(id),
  kind TEXT NOT NULL,                     -- hypothesis|gotcha|decision|constraint|deferred
  content TEXT NOT NULL,
  symbols TEXT NOT NULL DEFAULT '[]',     -- JSON: ["fn:parse_config","struct:Settings"]
  verdict TEXT NOT NULL DEFAULT 'unconfirmed'
    CHECK (verdict IN ('verified','failed','unconfirmed','overturned','stale')),
  verdict_source TEXT,
  embedding BLOB,                         -- float32[384] little-endian (Candle MiniLM)
  meta TEXT,                              -- JSON: для deferred — ceiling, upgrade_trigger, no_trigger bool
  created_at TEXT NOT NULL,
  stale_at TEXT                           -- момент авто-инвалидации (§5.4)
);

CREATE TABLE ast_changes (                -- AST-дельта итерации
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  run_id TEXT REFERENCES ralph_runs(id) ON DELETE SET NULL,
  iteration_id TEXT REFERENCES ralph_iterations(id) ON DELETE SET NULL,
  path TEXT NOT NULL,
  node_key TEXT NOT NULL,                 -- sha256(label|type|parent) — как в graph_nodes
  label TEXT NOT NULL,
  node_type TEXT NOT NULL,                -- function|class|interface|trait|method|…
  change_type TEXT NOT NULL,              -- added|removed|modified|moved
  sig_before TEXT, sig_after TEXT,
  loc_delta INTEGER DEFAULT 0,
  created_at TEXT NOT NULL
);

CREATE INDEX idx_ralph_iterations_run ON ralph_iterations(run_id, task_id, n);
CREATE INDEX idx_ralph_findings_proj ON ralph_findings(project_id, verdict);
CREATE INDEX idx_ast_changes_proj ON ast_changes(project_id, path);
CREATE INDEX idx_ast_changes_iter ON ast_changes(iteration_id);
```

### 4.3. Синхронизация PC↔VPS
`ralph_*` и `ast_changes` в бандлы синка **не включаются** (локальная история
циклов, как workspace/daily). Перенос знаний — опционально и явно:
CLI `ob2h ralph findings-to-memory --project <id>` конвертирует findings с
verdict=verified в memories. SYNC.md дополнить решением.

---

## 5. Контракт MCP-инструментов (7 новых)

Стиль существующих 25 инструментов: snake_case, компактные ответы (лимит
50 000 байт → `…[truncated]`), ошибки строкой `[Error] …`, конкурентный JSON-RPC.
Контракт v1 идентичен нормативной копии в Проекте A (Приложение D); при
изменении — фиксируется в обоих проектах одновременно.

| Инструмент | Аргументы | Возвращает / делает |
|---|---|---|
| `ralph_start` | project_id, feature_slug, goal, autonomy?, limits?, budget_tokens? | run_id; ошибка при активном run того же (project, feature) |
| `ralph_iteration` | run_id, task_id, n, hypothesis, plan, result, tests_summary, ladder_rung?, git_before?, git_after? | iteration_id, verdict, ast_changes_count, stale_marked; авто-вердикт (§5.1), AST-рескан → ast_changes, staleness-pass (§5.4) |
| `ralph_verdict` | iteration_id\|finding_id, verdict, verdict_source, note? | статус; история смен вердиктов |
| `ralph_context` | run_id, task_id, max_tokens?, mode? (lite\|full) | контекст-пакет (§6) |
| `ralph_report` | run_id? | сводка: прогресс, вердикты, AST-дельты, плато, debt-леджер, gain-метрики (§8) |
| `ast_diff` | project_id, from(iteration_id\|commit\|ts), to? | symbol-level дифф |
| `ast_history` | project_id, symbol | хронология символа по итерациям |

### 5.1. Правила авто-вердикта (`ralph_iteration`)
```
tests_summary.failed == 0 и passed > 0 → verified  (source=auto_tests)
tests_summary.failed > 0               → failed    (source=auto_tests)
tests_summary отсутствует              → unconfirmed
self_assessment из result — НИКОГДА не влияет (ADR-K4)
```

### 5.2. Инкрементальный AST-рескан
Существующий `ProjectService` (src/project/ast.rs, project_files sha256/mtime)
вызывается на `ralph_iteration` и по git-hook (hooks.rs). Diff — сравнение
канонических сигнатур узлов (kind|label|parent|params|returns) → ast_changes
(ADR-K3). Git-дифф не используется для семантики.

### 5.3. Debt-семантика (Ponytail)
- Писатель передаёт маркеры как findings `kind=deferred` с meta
  `{ceiling, upgrade_trigger}`; `ralph_report` собирает леджер.
- `upgrade_trigger` пуст → `no_trigger=true` (подсветка: такие «тихо гниют»).

### 5.4. Staleness-pass
Для каждого `ast_changes` (modified/removed): точный JSON-матчинг по символу
(пре-фильтр LIKE + точная проверка в коде) → `ralph_findings.verdict=stale,
stale_at=now` (если ещё не stale). `ralph_iterations` не помечаются — исторические
факты. Возврат числа помеченных в ответе.

---

## 6. Контекстный пакет (`ralph_context`) — с reuse-кандидатами

| Приоритет | Источник | Доля бюджета (full) |
|---|---|---|
| 1 | Фрагмент спеки задачи (читается ob2h из `projects.path` + `openspec/changes/<slug>/`) | до 30% |
| 2 | Негативный опыт: findings failed/overturned, релевантные символам задачи (гибрид FTS+вектор) | до 25% |
| 2.5 | **Reuse-кандидаты (Ponytail, ступень 2):** топ-N существующих символов по семантике задачи через `project_graph_search` (AST-граф + эмбеддинги) | до 15% |
| 3 | Инварианты: релевантные разделы `openspec/specs/` | до 10% |
| 4 | Архитектурная зона: god nodes / blast radius (`project_context`, `project_impact`) | до 10% |
| 5 | Проверенные решения: findings verified по теме; deferred с триггером | до 10% |

- `mode=lite`: приоритеты 1+2+2.5, лимит ×0.5; `full` — полный.
- Исключаются: stale/overturned (кроме явно указанных), история чата.
- Жёсткий лимит `max_tokens` (6000 по умолчанию); пакет целиком выгружается в
  `data/ralph/contexts/<run_id>/<task>-<n>.md`, в ответе `context_ref` (путь+sha256).
- ob2h читает репозиторий только для спеки и AST; в репозиторий не пишет.

---

## 7. Поведение сервиса

### 7.1. Dream-фаза 3 (новая, за autodream-гейтами)
Ночной цикл: выборка stale-findings → LLM-ревизия по текущему AST/спеке →
verified (source=dream) или подтверждение stale/decay; deferred без триггера —
кандидаты на эскалацию в память; сводка в dream_runs.stats. Выключаемо:
`OB2H_DREAM_RALPH=false`.

### 7.2. Скилл `ralph-loop` — процедура режима H
1. `ralph_start` по цели пользователя;
2. дельта-спека в `openspec/changes/<slug>/` (формат Fission-AI-совместимый);
3. на задачу: `ralph_context` → реализация штатными инструментами → тесты →
   `ralph_iteration` (с ladder_rung и маркерами) → ветвление по вердикту;
4. остановки (плато/лимиты) обязательны; red tests ≠ готово;
5. финал: `ralph_report`, архив спеки, `knowledge_extract` + `memory_save`.
Полный черновик — Приложение C.

### 7.3. doctor / backup / stats
- `ob2h doctor`: диагностика M4, сироты-применённые-раны (applying без апдейтов > N дней → warning).
- `omnes_backup`: новые таблицы включаются автоматически.
- `omnes_stats`: блок «ralph: runs/iterations/findings by verdict».

---

## 8. Gain-метрики и бенчмарк (заимствовано у Ponytail)

`ralph_report` добавляет блок gain:
- **repeat-failure rate** — доля итераций с повторным fingerprint падений (ценность негативного опыта);
- **reuse-hit rate** — доля задач, где reuse-кандидаты из AST использованы (фиксируется писателем как `ladder_rung=reuse:*`);
- **stale-оборачиваемость** — сколько знаний обновил staleness-pass/дрим;
- LOC/tokens на задачу (пишутся автором итерации).

Бенчмарк (в комплекте репо): fixture-репо, агentic-сессии Claude Code/Hermes,
сравнение «с ob2h-знаниями / без», контрольные армы и safety-тир — методика и
сырые цифры публикуются, как в benchmarks/ у Ponytail. Без честного измерения
домен не считается готовым.

---

## 9. Требования

| ID | Требование |
|---|---|
| FR-K1 | Миграция M4 §4; аддитивность; бэкап; не трогать graph_nodes |
| FR-K2 | 7 MCP-инструментов §5; контракт v1 стабилен для режимов H и O |
| FR-K3 | Авто-вердикты §5.1; self_assessment не влияет |
| FR-K4 | Reuse-кандидаты в `ralph_context` (§6) через project_graph_search; настраиваемый топ-N |
| FR-K5 | Staleness-pass §5.4; повторный вызов не перемечает |
| FR-K6 | Debt-семантика §5.3: kind=deferred, леджер, no-trigger |
| FR-K7 | Dream-фаза 3 §7.1; выключаемая |
| FR-K8 | Скилл `ralph-loop` ставится `ob2h skill install` во все harness; режимы lite/full |
| FR-K9 | `agent install --all` после апгрейда добавляет инструменты без ручных правок |
| FR-K10 | Gain-метрики в ralph_report + benchmark-пакет §8 |
| FR-K11 | Идемпотентность ralph_iteration по (run, task, n) |
| FR-K12 | ralph_* не синхронизируются бандлами; CLI findings-to-memory |
| NFR-K1 | Приватность: всё локально; сеть только LLM API |
| NFR-K2 | Инкрементальный рескан ≤ 2 c / 10k файлов; ralph_context ≤ 500 мс |
| NFR-K3 | Обратная совместимость v1.2.x; плагин Mode A parity |
| NFR-K4 | Windows/кириллические пути — обязательный тест-кейс |
| NFR-K5 | Полный аудит: контекст-пакеты на диске |

---

## 10. ADR (собственные, нумерация проекта B)

| ADR | Решение | Альтернатива | Обоснование |
|---|---|---|---|
| ADR-K1 | MCP stdio, без новых портов (сохранение ADR-7 ob2h) | HTTP-сервер | Проще жизненный цикл; harness уже так подключены |
| ADR-K2 | Один стор знаний: ob2h; omnesagent-kag в контуре не участвует | dual-write | Один источник правды (наследники одного движка, дубль разъедется) |
| ADR-K3 | AST-дельта по сигнатурам узлов, не git-дифф | git-based | Привязка к семантике кода; enables staleness по символам |
| ADR-K4 | Вердикт verified — только по объективным сигналам | Доверие самооценке LLM | Модели себя хвалят; это ломает ценность пометок |
| ADR-K5 | Reuse-кандидаты из AST-графа в контекст-пакете | Полагаться на поиск агента | Ponytail-ступень 2 становится детерминированной; у агента нет полного индекса |
| ADR-K6 | ralph_* вне синк-бандлов | Синхронизировать | Локальная история; знания переносятся явно через memory |

## 11. Этапы и приёмка (ob2h v1.3.0)

**Объём:** M4; 7 инструментов; staleness; контекст-пакет с reuse-кандидатами;
dream-фаза 3; скилл `ralph-loop`; doctor/backup/stats; CLI findings-to-memory;
benchmark-пакет.

**Приёмка:**
- [ ] `cargo test` зелёный; миграция на копии живой БД и на чистой; `confidence` колонка не тронута (тест схемы);
- [ ] интеграционный сценарий (Mode H, только MCP): 3 итерации (2 красные → 1 зелёная) → верные вердикты, ast_changes, stale-разметка при повторном изменении символа;
- [ ] `ralph_context` возвращает reuse-кандидатов из AST на fixture-репо (проверка: известный существующий символ попадает в пакет);
- [ ] debt: deferred-файндинг без триггера получает no_trigger, леджер в report;
- [ ] скилл `ralph-loop` установлен в Hermes + ZCode; мини-цикл (1 задача) пройден в чате Hermes по скиллу;
- [ ] `ob2h doctor --fix` зелёный; `omnes_backup` включает новые таблицы;
- [ ] кириллический путь проекта не ломает рескан (Windows-тест);
- [ ] benchmark-пакет: методика + первые цифры приложены.

## 12. Риски

| Риск | Мера |
|---|---|
| Потеря ручной колонки confidence при миграции | §4.1 запрет пересоздания + тест схемы |
| Режим H: модель ведёт цикл недисциплинированно | Скилл с жёсткой процедурой; вердикты только от ob2h |
| Ложные reuse-кандидаты (шумный семпоиск) | Топ-N мал; фильтр по node_type; писатель вправе игнорировать (это кандидаты) |
| LIKE/JSON-матчинг символов даст ложные stale | Пре-фильтр + точный JSON-парсинг; тесты |
| Рост ast_changes на длинных циклах | Личный масштаб; счётчик в stats; VACUUM через backup |
| Конкурентный доступ к ob2h.db | WAL + busy_timeout (уже); один активный run на (project, feature) |
| Коллизия записи одного key со стороны омнес-моста | LWW по updated_at + tombstone; namespace-ключи и эхо-защита (§3.4) |

## 13. Открытые вопросы
1. Публиковать ли ralph_* домен в open-source ob2h v1.3.0 (Pofium/ob2h)? Рекомендация: да.
2. Формат symbols: `kind:name` (агентам удобно) + опциональный node_key — ок?
3. Нужен ли рест-эндпоинт ob2h (план v0.x) для веб-клиентов, или MCP достаточно? Рекомендация: достаточно.

---

## Приложение A. Пример `ralph_iteration` (JSON полей)
```json
{
  "run_id": "01J8ZQ4V9C8X3K2M", "task_id": "T-002", "n": 2,
  "hypothesis": "i1 упала: mtime снимался до чтения, гонка с writer'ом. Чиним общую точку под lock.",
  "plan": ["stat+read под lock", "тест на гонку"],
  "result": {"what_done": "lock вокруг stat+read", "errors": [], "self_assessment": "ok"},
  "tests_summary": {"passed": 14, "failed": 0, "fingerprint": "sha256:9f2c…"},
  "ladder_rung": "reuse: std::sync::RwLock",
  "git_before": "a1b2c3d", "git_after": "e4f5a6b"
}
```

## Приложение B. Пример контекст-пакета (сокращённо)
```
<spec task="T-002">
WHEN конфиг меняется на диске THEN кэш инвалидируется за 1с
</spec>
<negative_experience>
- [failed, i1] mtime снимался до чтения — гонка с writer'ом (fn:invalidate_if_stale)
</negative_experience>
<reuse_candidates>
- fn:invalidate_if_stale (src/cache.rs) — существующий хук инвалидации
- struct:CacheManager (src/cache.rs) — god node, 12 вызовов
</reuse_candidates>
<invariants>
openspec/specs/config.md: горячее применение не требует рестарта процесса
</invariants>
<architecture_zone>
Blast radius fn:invalidate_if_stale: Medium — 3 вызова
</architecture_zone>
```

## Приложение C. Черновик SKILL.md `ralph-loop` (режим H)
```markdown
---
name: ralph-loop
description: Автономная реализация фичи циклами до зелёных тестов с накоплением
  опыта в ob2h. Use when пользователь просит «сделай автономно», «доломай тесты»,
  «реализуй по спеке в цикле».
---

[Правила минимальности — лестница; останавливайся на первой верной ступени
после понимания задачи и чтения кода:]
1. нужно ли вообще строить? 2. уже есть в кодовой базе — переиспользуй
3. stdlib 4. платформа 5. установленная зависимость 6. одной строкой
7. минимальный работающий код. Root cause, не симптом. Не отсекаемое:
валидация на границах, data-loss, безопасность, a11y, явно запрошенное.
Осознанное упрощение → комментарий `ponytail: <потолок>, <триггер>`.
Нетривиальная логика → ОДНА запускаемая проверка.

[Процедура:]
1. mcp__ob2h__ralph_start(project_id, feature_slug, goal).
2. Дельта-спека в openspec/changes/<slug>/ (proposal, specs/ GIVEN-WHEN-THEN,
   design.md с test_command, tasks.md T-001…).
3. На задачу: ralph_context → реализуй штатными инструментами (внимание на
   reuse_candidates!) → test_command → ralph_iteration(..., ladder_rung, маркеры
   как deferred-файндинги) → failed → новая итерация с учётом негативного опыта;
   3 одинаковых fingerprint подряд → СТОП и отчёт пользователю.
4. Все done → ralph_report → архив спеки → knowledge_extract + memory_save.
[Запрещено:] объявлять готово при красных тестах; обходить ralph_verdict;
работать без ralph_context; два активных run на одну фичу.
```
