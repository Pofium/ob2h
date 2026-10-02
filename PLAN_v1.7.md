# План разработки OB2H v1.7 — Расширение AST-сканера: веб-разметка, API-контракты, IaC, манифесты, спец-языки

> **Версия плана:** 1.7.0
> **Статус:** Черновик rev.1 — на утверждение владельцем
> **Обновлён:** 2026-10-02 — rev.1: адаптация «Фазы 6» плана `omnesagent-kag` (6.1–6.6) под ob2h.
> **Источник:** Фаза 6 плана omnesagent-kag («Расширение AST-сканера KAG новыми языками и форматами»),
> переработана под фактическую архитектуру ob2h (см. §5 — построчный маппинг 6.1–6.6).
> **Предыдущие этапы:** v1.0/1.1 (AST-граф), v1.2 (zero-config, watcher, semantic search, blast radius),
> v1.3 (bench, trust, PPR), v1.4 (typed edges, sync v2, трек C Ф36–40), v1.5.0 (`project_scan_status`),
> v1.6 (Ф41–46, System One/Laya — черновик, не утверждён).
> **Нумерация:** Ф**47–Ф53** (Ф41–46 зарезервированы за v1.6). Если v1.6 не будет утверждён —
> сквозная перенумерация по решению владельца; содержание фаз от этого не зависит.
> **Принцип совместимости:** контракт MCP — **36 инструментов без изменений** (новые форматы доступны
> через существующие `project_scan` / `project_scan_status` / `project_graph_search` / `project_call_path` /
> `project_impact` / `project_report` / `project_context`). Схема БД **не меняется** (`graph_nodes.node_type`
> и `graph_edges.label` — свободный TEXT, миграции нет). Все новые парсеры — детерминированные,
> **без LLM-вызовов** (ADR-2/ADR-1 сохраняются). Изменения аддитивные, с записью в `CHANGELOG.md`.
> **Предусловие старта:** закрытый DoD v1.4/v1.5 (выполнен: релиз 1.5.0, 36 инструментов, 18 расширений
> сканера), зелёные `cargo test` / `cargo clippy --all-targets`, свежий бэкап (`omnes_backup --scope quick`).

---

## 1. Мотивация

Сегодня сканер ob2h (`src/project/ast.rs`, 2941 строк) знает 18 расширений исходников: `rs py ts tsx js jsx go sql
c cpp cc cxx h hpp hxx php dart java cs kt kts swift rb vue svelte scala lua ex exs sh bash`. Он не видит три
класса сущностей, которые в реальных репозиториях владельца (`ob2h`, `presniakov.ru` — Laravel 12 + React/Vite,
`bitrix24-dgp`, `omnes-aibot`) несут структуру системы:

| Что не видно сейчас (факт по коду) | Где это бьёт | Чем закрываем |
|---|---|---|
| **Манифесты пакетов** (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `composer.json`) не читаются: `collect_files` (ast.rs:380) фильтрует по расширениям исходников, `DEPENDS_ON` рождается только на внутренних модулях | Внешние зависимости не имеют версий в графе; `project_impact` не отвечает «что сломает апгрейд пакета» | **Ф51** — `DEPENDS_ON` до узлов `pkg:<ecosystem>:<name>` с версией |
| **API-контракты** (`*.proto`, `*.graphql`, `openapi.{json,yaml}`) не парсятся: кросс-языковых связей нет, есть только эвристики Ф40 (`ROUTE` из декораторов, `QUERIES_TABLE` из SQLAlchemy) | Фронт↔бэк цепочка «fetch('/api/users') → хендлер → DTO» в `project_call_path` обрывается на границе языка | **Ф49** — узлы `Service`/`Endpoint`/`Message`/`Schema`, мосты с `provenance=INFERRED` |
| **IaC и контейнеризация** (`Dockerfile`, `compose.yaml`, `k8s/*.yaml`, `*.tf`) не парсятся; узлы `Dockerfile` нет даже как файла | Blast Radius не знает, что правка `Dockerfile`/деплой-манифеста затрагивает сервис; инфраструктурных хабов нет в `project_report` | **Ф50** — `Resource`/`Service`/`K8sResource`/`BaseImage` + `DEPENDS_ON` |
| **Веб-разметка и стили** (HTML/Pug/blade, CSS/SCSS) игнорируются: SFC-парсер (`parse_sfc`, ast.rs:2272) читает только `<script>`, `<template>`/`<style>` пропускаются | Связь «класс/селектор → компонент/шаблон» отсутствует; смена `.card` не показывает задетые места | **Ф48** — `CssClass`/`CssId`/`CssVar` + рёбра `STYLES`/`BINDS_CLASS` |
| **Спец-языки:** `.ipynb`, `.sol`, `.zig` | Ноутбуки и контракты/скрипты не в графе | **Ф52** |
| **Фильтр расширений триплицирован** — `collect_files` (ast.rs:380–412), `watcher::is_code_file` (watcher.rs:37–69) и маркеры `detect_manifest_metadata` (mod.rs:759) поддерживаются вручную; CHANGELOG уже фиксирует «фильтры расширений синхронизированы» как ручную процедуру | Каждое новое расширение = 3 правки + риск рассинхрона (watcher молча не пересканирует тип файла) | **Ф47** — единый реестр `LangSpec` |

Дополнительный эффект: Ф47–Ф53 дают «обратный» перенос в `omnesagent-kag` (в CHANGELOG уже есть пункт про
синхронный порт десяти языков) — но сам порт **не входит** в этот план (§7).

---

## 2. Архитектурный обзор

```
collect_files (ast.rs:348) ─┐                         [Ф47: реестр LangSpec — единственный список]
watcher::is_code_file ──────┤──> LangSpec::for_path(rel_path) ──> parse_file (ast.rs:433)
                            │                                      ├─ parse_html  [Ф48]
                            │                                      ├─ parse_css   [Ф48]
                            │                                      ├─ parse_proto / parse_graphql / parse_openapi [Ф49]
                            │                                      ├─ parse_terraform / parse_dockerfile / parse_k8s [Ф50]
                            │                                      ├─ parse_manifest (toml/json/txt/yaml) [Ф51]
                            │                                      └─ parse_notebook / parse_solidity / parse_zig [Ф52]
                            └──> AstScanResult { nodes, edges, file_hashes }

AstScanResult ──> scan_project (mod.rs:399, инкрементальность по sha256, delete-by-file)
                 ├─ префикс-таблица node_id → node_type (mod.rs:568 — сейчас только route:/table:)
                 ├─ resolve_calls (ast.rs:2810, type-pass) ──> provenance=RESOLVED
                 ├─ graph_nodes / graph_edges (схема без изменений)
                 └─ embed_unembedded_nodes (mod.rs:64) ──> PPR / graph_search

read-path, который обязан остаться честным:
  project_graph_search ── hybrid (FTS + вектор по узлам)
  project_call_path    ── USAGE_EDGE_LABELS (callpath.rs:13) → callers/callees/dead-code
  project_impact       ── analyze_impact (analytics.rs:353)
  project_report       ── update_god_nodes (analytics.rs:94) / communities.rs / repomap.rs
  blast-radius hint    ── blast.rs (TTL 30 мин, OB2H_EDIT_BLAST=warn)
  PPR                  ── DEFAULT_PPR_WEIGHTS (pagerank.rs:30), PPR_UNKNOWN_WEIGHT=0.5
```

**Границы, которые план не трогает:** `rusqlite`/SQLite (ADR-1), BLOB+numpy-вектора (ADR-2), никакой сети
кроме LLM/embedding API (ADR-5/§9 AGENTS.md), один пользователь, без tree-sitter в дефолтном пути.

---

## 3. Решения (ADR уровня плана)

| # | Решение | Альтернатива | Почему |
|---|---|---|---|
| **ADR-AST-1** | Новые парсеры — **регэксп/линейные экстракторы** в стиле существующих 20 (`parse_php`, `parse_java`, `parse_lua`…), а не tree-sitter | tree-sitter (как в omnesagent-kag) | Единый стек и стиль парсеров, нулевая стоимость обучения/knowledge-долга, отсутствие тяжёлых крейтов и C-сборки (ADR-8, §7). Существующие 18 языков доказывают достаточность: нам нужны узлы и рёбра, а не точное дерево. Точный AST — кандидат в бэклог, если появится требование семантического резолва |
| **ADR-AST-2** | **Единый реестр `LangSpec`** — источник истины для «что считать исходником»: `collect_files`, `watcher::is_code_file`, `parse_file` читают его | Три списка вручную (как сейчас) | Устраняет доказанный класс ошибок: расширение, добавленное в обходчик, но забытое в вотчере (или наоборот), молча выпадает из инкрементального обновления. См. CHANGELOG про C/C++ «собирались обходчиком, но молча не парсились» |
| **ADR-AST-3** | **Две новые зависимости**: `toml` (Cargo.toml / pyproject.toml) и поддерживаемый YAML-крейт (compose / k8s / openapi.yaml / pubspec.yaml). Если YAML-крейт не проходит ревизию — минимальный line-parser поверх подмножества (плоские мапы + списки), как у `.tf` | (а) писать всё регэкспом; (б) тянуть полный парсер | TOML/YAML регулярками — источник тихих ошибок (inline-таблицы, якоря, многострочные строки). Зависимости лёгкие, без транзитивных гигантов; **обязательна запись в `PLAN.md §6`** и проверка «нет новых транзитивных тяжёлых крейтов» |
| **ADR-AST-4** | **Матчинг манифестов по точному имени файла** (`package.json`, `openapi.yaml`, `Dockerfile`, `pubspec.yaml`), не по расширению; lock-файлы (`Cargo.lock`, `package-lock.json`, `pnpm-lock.yaml`, `poetry.lock`, `yarn.lock`) исключены явно; cap размера файла | Матч по расширению (`*.json`, `*.yaml`) | Иначе в граф попадают данные/локапы/CI-конфиги (в проектах владельца есть многомегабайтные `*.json`). Cap (по умолчанию 2 МБ, для `.ipynb` 5 МБ с отсечением `outputs`) защищает от `openapi.json` на десятки МБ |
| **ADR-AST-5** | **Словарь рёбер:** сначала переиспользуем существующие метки (`IMPORTS`, `CALLS`, `DEFINES`, `IMPLEMENTS`, `INHERITS`, `DEPENDS_ON`), новые (`STYLES`, `BINDS_CLASS`, `PROVIDES_RPC`, `SERVES_ENDPOINT`, `QUERIES_FIELD`, `USES_SCHEMA`, `MOUNTS`) — только когда семантика действительно новая. Каждая новая метка проходит явное решение по трём спискам: `USAGE_EDGE_LABELS` (callpath.rs:13), `DEFAULT_PPR_WEIGHTS` (pagerank.rs:30), `SYMBOL_TYPES` (callpath.rs:16) | Раздать каждой связи свою метку | `USAGE_EDGE_LABELS` управляет callers/callees, dead-code и blast-radius: если туда попадут `STYLES`/`BINDS_CLASS`, `dead-code` начнёт считать стиле-файлы «использованием», а blast-radius — шуметь. Новые метки по умолчанию **не** попадают в usage-список |
| **ADR-AST-6** | **Префиксы целевых узлов** (`route:` → `Route`, `table:` → `Table`, mod.rs:568–577) выносятся в таблицу реестра и расширяются: `service:`→`Service`, `endpoint:`→`Endpoint`, `contract:`→`Contract`, `schema:`→`Schema`, `pkg:`→`ExternalPackage`, `cssclass:`→`CssClass`, `cssvar:`→`CssVar`, `resource:`→`Resource`, `k8s:`→`K8sResource`, `image:`→`BaseImage` | Создавать узлы в каждом парсере | Сейчас авто-создание цели в `scan_project` — второе место знания о типах узлов; одна таблица нужна и `ast.rs`, и `mod.rs`, и `analytics` |
| **ADR-AST-7** | Контракт MCP **не меняется** (36 инструментов). Если по итогам Ф49–Ф51 понадобится фильтр (например, `project_scan` — по форматам), он добавляется как **опциональный аргумент** с записью в `CHANGELOG.md` и обновлением контракт-теста плагина | Новый инструмент на каждый формат | Правило «изменение контракта — только с записью в CHANGELOG» (§6 AGENTS.md); контракт-тест плагина (36) не должен ломаться |

---

## 4. Фазы

### Фаза 47 — Ядро: реестр языков, словарь узлов/рёбер, единые фильтры (1–1.5 дня)

Инфраструктурная фаза. Никаких новых форматов — только устранение трипликации и подготовка словарей, чтобы
Ф48–Ф52 были «добавь LangSpec + parse_* + тест».

- [ ] **47.1** `LangSpec` + таблица `LANGS` в `src/project/ast.rs`:
      `pub struct LangSpec { name: &'static str, matcher: Matcher, node_types: &'static [&'static str],
      edge_labels: &'static [&'static str], max_bytes: usize, parser: Parser }`,
      где `Matcher = Ext(&'static [&'static str]) | FileName(&'static [&'static str]) | Predicate(fn(&str)->bool)`,
      `Parser` — тег для диспетчера (не fn-указатель: `parse_*` берут `&self`). Равноценная замена существующего
      `match ext.as_str()` в `parse_file` (ast.rs:453) на `LANGS.iter().find(...)`; поведение 18 расширений — 1:1.
- [ ] **47.2** Замена дублирующих списков: `collect_files` (ast.rs:378–427) и `watcher::is_code_file`
      (watcher.rs:14–73) → `LangSpec::for_path(rel_path).is_some()`; список исключённых каталогов (`target`,
      `node_modules`, `dist`, `build`, `venv`, `.venv`, `vendor`, `data`, `logs`, `backups`, `__pycache__`) — тоже
      один `const SKIP_DIRS` (сейчас два расходящихся списка: ast.rs:364 и watcher.rs:17).
- [ ] **47.3** Словарь типов: обновить doc-контракт `AstNode.node_type` (ast.rs:16) и `AstEdge.label` (ast.rs:28)
      до полного перечисления v1.7 (текущее — «`Module|Struct|Class|Interface|Function|Table|File`» и
      «`IMPORTS|CALLS|IMPLEMENTS|DEFINES|DEPENDS_ON|FOREIGN_KEY_TO`» — устарело уже сейчас: нет `INHERITS`,
      `ROUTE`, `QUERIES_TABLE`). Завести таблицу `NODE_TYPE_BY_PREFIX` (см. ADR-AST-6) и использовать её
      в `scan_project` (mod.rs:568) вместо `if/else`.
- [ ] **47.4** Явные решения по трём спискам (`USAGE_EDGE_LABELS` callpath.rs:13, `SYMBOL_TYPES` callpath.rs:16,
      `DEFAULT_PPR_WEIGHTS` pagerank.rs:30): задокументировать в комментариях, что входит и почему; для
      существующих кодовых рёбер (`IMPORTS`/`CALLS`/`DEFINES`/`IMPLEMENTS`/`INHERITS`) задать веса PPR
      явно (сейчас они падают в `PPR_UNKNOWN_WEIGHT = 0.5`) — **значения — только после замера Ф53.3**, до этого
      за флагом, чтобы не сдвинуть pending-поиск вслепую.
- [ ] **47.5** Скелет тестов: новый `tests/test_ast_langs.rs` — по одной минимальной фикстуре на формат
      (конкатенация строк в коде, как в `test_ast_extractor.rs`) + хелпер `assert_has_node(res, label)`,
      `assert_has_edge(res, label)`; отдельный тест-инвариант **консистентности реестра**:
      для каждого `LangSpec` `parse_file` на минимальном содержимом не паникует, даёт ≥1 узел типа `File`
      и только метки из `edge_labels[i]`.
- [ ] **47.6** Защита от регрессии: прогон существующих 16 тестов `test_ast_extractor.rs` +
      `test_incremental_scan.rs` + `test_watcher.rs`; сверка «до/после» по узлам и рёбрам на фиксированном
      репозитории (`ob2h` self-scan: число узлов/рёбер до и после рефактора — должно совпасть 1:1).

**DoD:** `grep -rn` не находит дублирующих whitelist'ов расширений; `cargo test` + `cargo clippy --all-targets`
зелёные; self-scan `ob2h` до/после рефактора даёт идентичные nodes/edges; тест консистентности реестра зелёный.

---

### Фаза 48 — Веб: HTML, CSS/SCSS, связи компонентов и стилей (6.1) (1.5–2 дня)

- [ ] **48.1** `parse_html` (в стиле `parse_php`): теги верхнего уровня с `id` → узлы `Element`
      (`el:`-префикс, label = тег#id), `<script src="x">` → `IMPORTS` (в `file:...` если локальный путь,
      иначе `ExternalModule`), `<link rel="stylesheet" href="x">` → `STYLES` (→ `css:`-узел),
      `class="a b"` → `BINDS_CLASS` (→ `cssclass:a`, `cssclass:b`, INFERRED: связь по имени класса),
      `<form action>`/`hx-post`/`href` → `ROUTE` (INFERRED, тем же способом, что Ф40.2 ast.rs:614) —
      это даёт Laravel blade/Bitrix-шаблонам связь с эндпоинтами.
- [ ] **48.2** `parse_css`: `selectors` — `.foo` → `CssClass`, `#bar` → `CssId`; `@import "x"` → `IMPORTS`;
      `--custom-prop: value` → `CssVar` (DEFINES); `@media/@supports` — не создаём узлов (шум);
      вложенность и `@include`/миксины SCSS **не резолвим** (бэклог §8). Файлы-цели `@import` без расширения —
      нормализация `x` → `x`/`x.css`/`x.scss` (первое существующее).
- [ ] **48.3** SFC Vue/Svelte: `parse_sfc` (ast.rs:2272) дополнить разбором `<template>` и `<style>`:
      `<style>` → `parse_css` с `line_offset` (механика уже есть у `parse_ts_js`), `<template>` — упрощённый
      HTML-разбор (классы → `BINDS_CLASS`, компоненты по тег-имени с заглавной буквы → `IMPORTS`).
- [ ] **48.4** Узлы/рёбра новой фазы: `File`(css/html), `Element`, `CssClass`, `CssId`, `CssVar`;
      рёбра `STYLES`(EXTRACTED), `IMPORTS`(EXTRACTED), `BINDS_CLASS`(INFERRED), `ROUTE`(INFERRED, переиспользуем),
      `DEFINES`(file → cssvar/class). Новые метки **не** добавляются в `USAGE_EDGE_LABELS` (ADR-AST-5);
      веса PPR — решение Ф53.3.
- [ ] **48.5** Cap и производительность: css/html парсятся до 2 МБ; для файлов с > 5000 строк css —
      агрегировать классы по файлу (один узел на класс, дедуп по имени) и не создавать узел на каждый селектор.
      Тест на дедуп: 200 правил с `.card` → один узел `cssclass:card`.
- [ ] **48.6** Тесты (в `tests/test_ast_langs.rs`): HTML с id/class/script/link; CSS с классом/id/@import/var;
      SFC с template+style (номера строк — относительно всего файла, как в Ф10 для `parse_ts_js`);
      «обычный `.yml` не парсится» не относится сюда — здесь: «обычный текст с `<` не ломает парсер».

**DoD:** на реальном проекте владельца (`C:\Projects\self-site-tbot-ai\self-site\frontend` и Laravel `views`)
`project_graph_search("cssclass:card")` находит узел с описанием файла/строки, `project_impact("App.css")`
перечисляет компоненты/шаблоны с `BINDS_CLASS`; регрессия существующих тестов — ноль.

---

### Фаза 49 — API-контракты: proto, GraphQL, OpenAPI (6.2) (2 дня)

- [ ] **49.1** `parse_proto`: `package` → `Module`, `service X` → `Service`, `rpc Name(...) returns (...)` →
      `Function` (+`DEFINES` service→rpc), `message M` → `Struct`, `enum E` → `Enum`, `import "x.proto"` →
      `IMPORTS`, целевые типы полей (`message M { T f = 1; }`) → `USES_SCHEMA`(EXTRACTED, если тип объявлен
      в этом же скане; иначе `ExternalModule`). Версии опций (`option (version) = ...`) — в description.
- [ ] **49.2** `parse_graphql`: `type Name` → `Struct`, `interface` → `Interface`, `enum` → `Enum`,
      `input` → `Struct` (+пометка в description), `schema { query: Q }` → `Schema`,
      `type Query { field: T }` / `Mutation` / `Subscription` → `Function` с меткой `Query`/`Mutation` в описании;
      `extend type` — мержим в тот же узел (дедуп по имени).
- [ ] **49.3** `parse_openapi` (json через `serde_json`, yaml — через YAML-крейт ADR-AST-3): путь+метод →
      узел `Endpoint` (**и одновременно** `route:`-узел — см. ниже), `components/schemas/*` → `Struct`,
      `info.title`/`version` → description файла, `$ref` → `USES_SCHEMA`(EXTRACTED),
      `parameters`/`requestBody` → `Field` (DEFINES endpoint→field), `security`/`servers` — в description.
      **Важно:** каждый endpoint дополнительно создаёт `route:<METHOD /path>` с меткой `ROUTE`, чтобы
      заработали существующие entrypoint-правила (`callpath.rs:301`, dead-code исключает `ROUTE`-цели) —
      иначе хендлеры бэка начнут считаться мёртвым кодом.
- [ ] **49.4** Мосты INFERRED между контрактом и кодом (по образцу Ф40.2, `provenance=INFERRED`):
      (а) серверный роут/декоратор ↔ endpoint из OpenAPI — по нормализованному пути (совпадение точное,
      шаблоны `{id}` ↔ `:id`/`<id>` нормализуются в `:param`, иначе связь не создаётся);
      (б) клиентский вызов HTTP (`fetch`/`axios.get`/`requests.get` в ts/py) → `SERVES_ENDPOINT` к
      `route:`-узлу — только при точном совпадении пути (литерал или конкатенация с шаблоном);
      (в) GraphQL-запрос (строка `gql`/`graphql` в ts/py) → `QUERIES_FIELD` к `Function`-полю;
      (г) proto `rpc` ↔ серверный хендлер по имени метода (совпадение snake_case ↔ camelCase — тривиальная
      нормализация). Всё это — только точные совпадения; порог зафиксировать тестом на «шумных» фикстурах
      (похожие, но разные пути `/api/user` vs `/api/users` — связи нет).
- [ ] **49.5** Тесты: `.proto` (service/rpc/message/enum/import), `.graphql` (type/interface/enum/query/
      mutation/extend), `openapi.json` + `openapi.yaml` (paths/schemas/$ref/parameters) — один и тот же
      семантический результат для json и yaml (тест-паритет); мосты: `fetch('/api/users')` ↔
      `openapi path /api/users` → `SERVES_ENDPOINT`(INFERRED); негативный тест на несовпадение путей.

**DoD:** на проекте с OpenAPI (или синтетическом эталоне в `tests/fixtures/`) `project_call_path` проходит
цепочку фронт-функция → `route:` → серверный хендлер через INFERRED-рёбра; `project_impact("openapi.yaml")`
показывает эндпоинты/DTO; хендлеры, описанные в контракте, не попадают в `project dead-code`.

---

### Фаза 50 — IaC и контейнеризация: Terraform, Docker, K8s/Helm (6.3) (2 дня)

- [ ] **50.1** `parse_terraform` (`.tf`, `.hcl`; регэксп+блочный сканер, без `hcl-rs` — ADR-AST-1):
      `resource "type" "name"` → `Resource` (провайдер из префикса типа: `aws_*`, `yandex_*`, `google_*` —
      в `description`), `module "n" { source = ... }` → `Module` + `DEPENDS_ON` (локальный `./modules/x` →
      `file:`-связь, внешний — `ExternalModule`), `variable`/`output` → `Variable`/`Output`,
      `data "type" "name"` → `DataSource`, `depends_on = [a.b]` → `DEPENDS_ON` (внутрипроектный резолв по
      `type.name` → узел `Resource`). Провайдеры и `backend` — в description файла, без узлов (шум).
- [ ] **50.2** `parse_dockerfile` (имя файла `Dockerfile`, `*.Dockerfile`, `Dockerfile.*`): `FROM img:tag` →
      `BaseImage` (`image:`-префикс) + `DEPENDS_ON` с версией в контексте, `ARG`/`ENV` → `EnvVar` (DEFINES),
      `COPY`/`ADD` локальных путей → `DEPENDS_ON` к `file:`-узлу (внешние URL — `ExternalModule`),
      `WORKDIR`/`EXPOSE`/`CMD`/`ENTRYPOINT` — в description. Многоступенчатые сборки: `FROM ... AS stage` —
      узлы стейджей не создаём, только базовые образы.
- [ ] **50.3** `parse_compose` (`docker-compose.yml`, `compose.yaml`, `compose.yml`): `services.<name>` →
      `Service` (`svc:`-префикс) + `File`-узел; `image:` → `BaseImage` + `DEPENDS_ON` (+ версия),
      `build:` → `DEPENDS_ON` к `Dockerfile`/каталогу, `depends_on` → `DEPENDS_ON` (резолв по имени сервиса),
      `environment`/`env_file` → `EnvVar`, `ports` → `Port` (DEFINES), `volumes` → `Volume` (MOUNTS),
      `networks` → `Network` (MOUNTS). Порты/тома — по одному узлу на уникальное значение.
- [ ] **50.4** `parse_k8s` (`k8s/**/*.yaml`, `deploy/**/*.yaml`, Helm `values.yaml`, `Chart.yaml`): документы,
      содержащие `kind:` → `K8sResource` (`k8s:`-префикс, label = `kind/name`, описание = namespace+kind),
      `image:` → `BaseImage` + `DEPENDS_ON` (пин версии виден), `configMapRef`/`secretRef`/`envFrom` →
      `DEPENDS_ON` к соответствующим `ConfigMap`/`Secret`-узлам (резолв по имени+namespace, если объявлены
      в этом же репозитории), `serviceAccountName`/`ingress → service` → `DEPENDS_ON`, `selector`/`labels` →
      `BINDS_SELECTOR`(INFERRED: selector ↔ labels внутри репо). Helm-шаблоны (`{{ }}`) **не рендерим**:
      узел получает пометку `templated` в description; рендер с `values.yaml` — бэклог §8.
- [ ] **50.5** Классификатор «что это за YAML»: файл YAML сканируется как k8s/compose/openapi только при
      наличии характерного корневого ключа (`kind:`, `services:`, `openapi:`/`swagger:`) — иначе игнор
      (важно: CI-конфиги `.github/workflows/*.yml` содержат `jobs:`, а не `services:` — их не разбираем).
      Тест: обычный `application.yaml`/CI-workflow не порождает ни одного узла.
- [ ] **50.6** Тесты: Dockerfile (multi-stage, ARG/ENV, COPY), compose (image/build/depends_on/environment/
      ports/volumes), k8s (Deployment+Service+Ingress+ConfigMap, image пин), Helm values+Chart, `.tf`
      (resource/module/variable/output/depends_on, локальный module source → `file:`-связь), негативные
      (CI-workflow, `application.yaml`).

**DoD:** на `bitrix24-dgp`/`presniakov.ru` в графе появляются узлы `BaseImage`/`Service`/`K8sResource`;
`project_impact("Dockerfile")` перечисляет сервисы/деплойменты, зависящие от образа; `project_report`
содержит инфраструктурные хабы; ложных узлов из CI-конфигов/`application.yaml` — ноль.

---

### Фаза 51 — Манифесты пакетов и граф зависимостей (6.4) (1.5 дня)

- [ ] **51.1** Парсеры (ADR-AST-3, ADR-AST-4):
      `Cargo.toml` — `[dependencies]`/`[dev-dependencies]`/`[build-dependencies]` (+`workspace = true` →
      связь с workspace-манифестом), `[workspace] members`; `package.json` — `dependencies`/
      `devDependencies`/`peerDependencies` (+`scripts` → `Script`-узлы первого уровня, полезно для
      `project_graph_search("script:build")`); `pyproject.toml` — PEP 621 `[project].dependencies` +
      `[tool.poetry.dependencies]`/`[dependency-groups]`; `requirements.txt` — только строки с версией
      (`==`, `~=`, `>=`, `<`), строки без версии — `DEPENDS_ON` с пометкой `unpinned`;
      `go.mod` — `require`/`replace`/`exclude`; `pubspec.yaml` — `dependencies`/`dev_dependencies`;
      `composer.json` — `require`/`require-dev`.
- [ ] **51.2** Рёбра и узлы: `DEPENDS_ON` от `File`-узла манифеста к `pkg:<ecosystem>:<name>`
      (`ExternalPackage`); **версия и требование** — в `context` ребра и в `description` узла
      (`serde_json 1.0`, `react ^18.2.0`, `golang.org/x/net v0.28.0`); `weight`: 1.0 при точном пине
      (`=`/`==`/exact semver), 0.5 при диапазоне (`^`/`~`/`>=`/`*`), 0.3 для dev/peer-зависимостей.
      Для `Cargo.toml` git/path-зависимости: `path = "../x"` → `DEPENDS_ON` к `file:`-узлу (своя
      рабочая область — реальный внутренний граф!), `git = ...` → `ExternalModule` с URL в контексте.
- [ ] **51.3** Консолидация внешних пакетов: импорты кода (`use serde_json`, `import React from 'react'`,
      `import requests`) сейчас рождают узлы `ExternalModule` **без версий** — нормализовать и слить с
      `pkg:`-узлами: rust `use a::b` → `pkg:rust:a`; python `import a.b` / `from a.b import c` →
      `pkg:pypi:a`; ts/js `from 'a/b'` → `pkg:npm:a` (scope `@scope/pkg` сохраняется);
      go `import "a/b/c"` → `pkg:go:a/b/c`; php `use A\B\C` → `pkg:composer:a/b`;
      dart `package:a/b.dart` → `pkg:pub:a`. Слияние — на этапе `scan_project` (одна функция нормализации
      в реестре: `package_of_module(lang, module) -> Option<String>`), не в парсерах.
- [ ] **51.4** tech_stack (`detect_manifest_metadata`, mod.rs:759): добавить маркеры — `openapi.yaml|json` →
      `openapi`, `*.proto` → `protobuf`, `*.graphql|*.gql` → `graphql`, `Dockerfile|compose.yaml|
      docker-compose.yml` → `docker`, `*.tf|*.tfvars` → `terraform`, `k8s/|Chart.yaml` → `kubernetes`,
      `*.ipynb` → `jupyter`, `*.sol` → `solidity`, `build.zig` → `zig`. Маркеры Solidity/Zig/Jupyter
      добавлять только при ≥2 файлах формата (иначе единственный случайный `.sol` в vendor-подобном месте
      искажает описание проекта).
- [ ] **51.5** Тесты: по одному манифесту каждого экосистемного типа (включая git/path-зависимость Cargo);
      тест «`serde_json` из `Cargo.toml` + `use serde_json` из кода → **один** узел `pkg:rust:serde_json`
      с версией из манифеста»; тест «lock-файлы игнорируются»; тест «`requirements.txt` с `unpinned` и
      комментариями/`-r other.txt` не ломается»; тест tech_stack-маркеров.

**DoD:** `project_graph_search("pkg:rust:serde_json")` отдаёт узел с версией и потребителями;
`project_impact("Cargo.toml")` показывает файлы, зависящие от изменённого пакета (в т.ч. внутренние
`path`-зависимости); скан не срывается и не тратит время на lock-файлы (замер в Ф53.3).

---

### Фаза 52 — Специализированные языки: Jupyter, Solidity, Zig (6.5) (1 день)

- [ ] **52.1** `.ipynb`: `serde_json` → обход `cells[]`, взять только `cell_type == "code"`, склеить
      `source[]` (массив строк); **`outputs` не читать** (там base64-картинки — риск памяти);
      `parse_python` получает `line_offset` (сигнатуру дополнить — сейчас у `parse_python` (ast.rs:635) его
      нет, в отличие от `parse_ts_js`), номера строк — относительно всего `.ipynb` (для навигации в графе);
      узел `Notebook` + `DEFINES`; cap 5 МБ; при отсутствии `cells` — только `File`-узел с пометкой.
- [ ] **52.2** `.sol`: `contract C is A, B` → `Class` (+ `INHERITS` для первой базы, `IMPLEMENTS` для
      остальных — как в PHP/Java-парсерах), `interface` → `Interface`, `library` → `Module`,
      `function`/`modifier`/`event`/`error` → `Function` (тип в description), `mapping(...)` → `Mapping`
      (DEFINES), `import "x.sol"` → `IMPORTS`, `using X for Y` → `USES_SCHEMA`, `constructor` → `Function`.
- [ ] **52.3** `.zig`: `pub fn`/`export fn`/`fn`/`inline fn` → `Function` (в т.ч. `fn name(...) Type {}`),
      `struct`/`enum`/`union`/`opaque` → `Struct`, `const X = struct {` → `Struct` (частая идиома Zig),
      `@import("std")`/`@import("mod.zig")` → `IMPORTS` (`pkg:zig:std` / `file:`), `test "name"` →
      `Function` с label `test:<name>` (существующее правило `entrypoint_reason`, callpath.rs:301, уже
      выводит тестовый код из dead-code по префиксу имени — совместимо), `var`/`const` верхнего уровня —
      `Variable` только если `pub`.
- [ ] **52.4** Тесты: `.ipynb` с двумя code-ячейками (проверка склейки, offset, отсутствия утечки
      `outputs`), «ipynb без `cells`» (только File), `.sol` (наследование, mapping, event, import),
      `.zig` (pub fn, const-структура, @import, test-блок).

**DoD:** ноутбук из реального репозитория (или фикстура 5 МБ с base64-выводами) сканируется без роста
памяти и без узлов из `outputs`; функции из ячеек находятся в `project_graph_search`; `.sol`-контракт с
`is` даёт `INHERITS`-ребро; zig `test`-блоки не попадают в `project dead-code`.

---

### Фаза 53 — Интеграция в CodeGraph, валидация, релиз (6.6 + доки) (1.5–2 дня)

- [ ] **53.1** Проверка read-path на новых узлах/рёбрах (это и есть «интеграция с graph search», 6.6.3):
      `project_graph_search` — новые узлы эмбеддятся через `embed_unembedded_nodes` (mod.rs:64); проверить,
      что описания `pkg:`/`CssClass`/`Endpoint` не «размывают» семантический поиск (замер recall до/после);
      `project_call_path`/`callers`/`callees` — убедиться, что новые метки не попали в `USAGE_EDGE_LABELS`
      случайно (иначе callers бэкенд-функции начнут показывать CSS-классы);
      `project_impact`/`analyze_impact` (analytics.rs:353) — проверка радиуса по манифестам и стилям;
      `update_god_nodes` (analytics.rs:94) + `communities.rs` — риск: `pkg:`-узлы с большой in-degree станут
      «богами» и выдавят реальные функции: решение — исключить `ExternalPackage`/`BaseImage`/`CssClass` из
      кандидатов в god nodes (или понизить вес их рёбер); `repomap.rs` — оставить в repo-map только файлы кода
      (манифесты/стили не подмешивать в token-budget карту).
- [ ] **53.2** Blast Radius (сценарии владельца): правка `*.css` → warn-блок перечисляет компоненты/шаблоны
      (`BINDS_CLASS`); правка `Cargo.toml`/`package.json` → потребители обновляемого пакета; правка `Dockerfile`/
      `compose.yaml` → сервисы; правка `openapi.yaml` → эндпоинты/хендлеры. Проверка под `OB2H_EDIT_BLAST=warn`
      через MCP-ресурс `project://current/blast-radius` и plugin-мост в prefetch (как в Ф38.1).
- [ ] **53.3** Метрики и гейты (ADR-13 + ADR-AST-5):
      **bench** — `ob2h bench` до/после (golden 36 кейсов, recall@5/MRR): регрессия по recall запрещена
      (сравнение в `docs/bench_baseline.md`); **скан** — таблица по трём репозиториям (`ob2h`,
      `self-site-tbot-ai/self-site`, `bitrix24-dgp`): files/nodes/edges/БД-прирост/время полного и
      инкрементального скана (p50/p95) до и после; гейт: p95 инкрементального рескана не хуже +20%,
      прирост узлов — с осознанной оценкой (класс CSS = узел: для больших фронтов агрегируем, см. 48.5);
      **веса PPR** — калибровка `DEFAULT_PPR_WEIGHTS` по этим замерам (только после данных, дефолт-флаг
      до этого). Числа — в `docs/PLAN_v1.7_bench.md`.
- [ ] **53.4** Инкрементальность/watcher: новые расширения попадают в вотчер через реестр (Ф47.2) —
      тест «правка `package.json` → DEPENDS_ON-рёбра обновились; удаление зависимости → ребро исчезло
      (delete-by-file в `scan_project`)»; тест «`git status` в чужом репо при сканировании не нужен» (скан
      остаётся локальным и без сети).
- [ ] **53.5** Документация и релиз:
      `docs/ARCHITECTURE.md` — таблица словаря `node_type`/`edge_label` (+ раздел про реестр `LangSpec`);
      `CHANGELOG.md` — запись v1.7.0 (форматы, узлы/рёбра, зависимости, отсутствие изменений контракта);
      `README.md` — упоминание поддерживаемых форматов, если там есть список языков; `Cargo.toml` → 1.7.0;
      **оба скилла синхронно**: `skills/ob2h/SKILL.md` и `skills/ob2h-zcode/SKILL.md` (список форматов,
      новые питфоллы, примеры `project_graph_search("pkg:...")`/`project_impact("openapi.yaml")`);
      `PLAN.md §6` — запись о v1.7 и, при отклонениях от плана, о самих отклонениях.
- [ ] **53.6** Приёмочный чеклист: `cargo test` (все, включая новые `test_ast_langs.rs`), `cargo clippy
      --all-targets`, `cargo fmt --check` — зелёные; контракт-тест плагина (36 инструментов) не изменён;
      снапшот `tools/list` совпадает; self-scan `ob2h` и «боевого» проекта проходит end-to-end;
      построчный маппинг §5 отмечен.

**DoD:** все гейты 53.3 пройдены и записаны числами; регрессия recall — ноль; контракт 36 без изменений;
доки/скиллы/CHANGELOG актуальны; `PLAN.md §6` дополнен; чекбоксы §5 (6.1–6.6) закрыты.

---

## 5. Маппинг исходного плана omnesagent-kag → ob2h

| Исходная задача (Фаза 6 omnesagent-kag) | ob2h-фаза | Что меняется при адаптации |
|---|---|---|
| 6.1.1 AST-парсер HTML (DOM, теги, id/class, `<script src>`, `<link rel=stylesheet>`) | **Ф48.1** | DOM-дерево не строим — узлы/рёбра прямо из разметки (ADR-AST-1); `<form action>`/ссылки дополнительно дают `ROUTE` |
| 6.1.2 CSS/SCSS (классы, ID, `@import`, custom properties) | **Ф48.2** | Без резолва вложенности/миксинов (бэклог §8) |
| 6.1.3 Граф стилей и компонентов (`AstNode` компонент/шаблон, рёбра `STYLES`, `IMPORTS`, `BINDS_CLASS`) | **Ф48.3–48.4** | `BINDS_CLASS` = INFERRED (связь по имени), в `USAGE_EDGE_LABELS` не входит |
| 6.2.1 Protocol Buffers (`service`, `rpc`, `message`, `enum`, связка SDK↔gRPC) | **Ф49.1, 49.4г** | Связка SDK↔сервер — INFERRED по нормализованному имени метода |
| 6.2.2 GraphQL (`type`, `query`, `mutation`, `schema`, связка запрос↔резолвер) | **Ф49.2, 49.4в** | `QUERIES_FIELD`(INFERRED) вместо магии резолвера |
| 6.2.3 OpenAPI/Swagger (эндпоинты, DTO, параметры/ответы) | **Ф49.3** | JSON+YAML паритет; эндпоинт **дополнительно** создаёт `route:`-узел (совместимость с Ф40/dead-code) |
| 6.3.1 Terraform/OpenTofu (`resource`, `module`, `variable`, `output`, провайдеры) | **Ф50.1** | Провайдер — в description, не отдельный узел (шум) |
| 6.3.2 Docker & Compose (сервисы, порты, env, `depends_on`) | **Ф50.2–50.3** | + `BaseImage` с пином версии, `MOUNTS` для volumes/networks |
| 6.3.3 Kubernetes/Helm (`Deployment`, `Service`, `Ingress`, `ConfigMap`) | **Ф50.4** | Helm-шаблоны не рендерим (пометка `templated`); классификатор YAML обязателен (50.5) |
| 6.4.1 Парсеры манифестов (`Cargo.toml`, `package.json`, `pyproject/requirements`, `go.mod`, `pubspec.yaml`, `composer.json`) | **Ф51.1** | + точный матч по имени файла, lock-файлы исключены, cap 2 МБ |
| 6.4.2 `AstEdge::DEPENDS_ON` с точными версиями | **Ф51.2–51.3** | Версия в `context`/`description`; **слияние** с существующими `ExternalModule` из импортов (в исходном плане этого не было) |
| 6.5.1 Jupyter (`.ipynb`, `cells[type=code]`) | **Ф52.1** | `outputs` отсекаются; нужен `line_offset` у `parse_python` |
| 6.5.2 Solidity (`contract`, `interface`, `library`, `modifier`, `event`, `function`, `mapping`) | **Ф52.2** | `mapping` → отдельный тип узла `Mapping` |
| 6.5.3 Zig (`fn`, `struct`, `const`, `@import`) | **Ф52.3** | `test "..."` → тестовые узлы, совместимые с dead-code |
| 6.6.1 Регистрация в диспетчере `ast/mod.rs` (`scan_file`/`scan_directory`) | **Ф47.1–47.2** | У нас один `ast.rs` (не каталог `ast/`), плюс **три** точки (обходчик, вотчер, диспетчер) → сводятся в реестр `LangSpec` |
| 6.6.2 Юнит-тесты на каждый парсер (`crates/omnesagent-kag/tests`) | **каждая фаза, файл `tests/test_ast_langs.rs`** | Один файл-набор + инвариант консистентности реестра (47.5) |
| 6.6.3 Интеграция с graph search + пересчёт Blast Radius | **Ф53.1–53.2** | Дополнительно: god nodes/communities/repomap-политика для новых типов узлов, bench-гейт ADR-13 |

---

## 6. Что НЕ делаем (границы и отложенное)

- **tree-sitter / настоящий AST** — ADR-AST-1 (бэклог: если понадобится семантический резолв).
- **Рендер Helm-шаблонов** с подстановкой `values.yaml` (узел с пометкой `templated`).
- **Полный CSS-каскад**: специфичность, наследование, `@include`/миксины SCSS, `@apply` Tailwind —
  связь только по имени класса/файла.
- **Резолв бандлеров фронтенда** (vite/webpack alias, tsconfig paths) — бэклог; без него часть
  `IMPORTS` останется `ExternalModule`.
- **Workspace-связи npm/pnpm** (`workspaces:*`) — только базовая обработка; Cargo-workspace — `path`-зависимости.
- **Новые MCP-инструменты** и изменения схемы БД — ADR-AST-7.
- **LLM-вызовы в парсерах** — запрещено (детерминизм, §7 AGENTS.md).
- **Порт в omnesagent-kag** — по отдельному решению владельца (CHANGELOG фиксирует предыдущий обратный порт).

---

## 7. Риски

| Риск | Вероятность | Митигация |
|---|---|---|
| Шум: тысячи CSS-классов/`pkg:`-узлов размывают семантический поиск и god nodes | высокая | Агрегация css-классов по файлу (48.5); исключение инфраструктурных типов из кандидатов в god nodes (53.1); bench-гейт recall (53.3) |
| YAML/TOML-крейт: транзитивные зависимости, статус поддержки (`serde_yaml` архивирован) | средняя | ADR-AST-3: выбор поддерживаемого крейта (или минимальный line-parser), проверка `cargo tree` на тяжёлые зависимости, запись в `PLAN.md §6` |
| Производительность: `openapi.json`/большие манифесты + regex по каждому файлу | средняя | Cap по размеру (2 МБ / 5 МБ для ipynb), ранний выход при отсутствии характерных ключей, замер p95 (53.3) с гейтом +20% |
| Ложные срабатывания классификатора YAML (CI-конфиги → k8s-ресурсы) | средняя | Обязательный признак (`kind:`, `services:`, `openapi:`) + негативные тесты (50.5–50.6) |
| Новые символы попадают в `project dead-code` (нет usage-рёбер) | средняя | ADR-AST-5: `SYMBOL_TYPES` и `USAGE_EDGE_LABELS` меняются только осознанно; `route:`-узлы для эндпоинтов (49.3); тест «после скана проекта с OpenAPI dead-code не растёт» |
| Рассинхрон фильтров: расширение есть в обходчике, нет в вотчере | низкая (после Ф47) | Реестр `LangSpec` — единственный источник, тест консистентности (47.5) |
| Инкрементальная дельта ломается при появлении новых расширений (у существующих проектов новый набор файлов) | средняя | Первый скан после апгрейда — полный (`project_scan` без incremental), затем инкремент; проверить delete-by-file для манифестов (53.4) |
| Регрессия recall (ADR-13) из-за новых узлов в векторах | средняя | Обязательный `ob2h bench` до/после, числа в `docs/PLAN_v1.7_bench.md`, при регрессии — исключение узлов из эмбеддинг-пула (по типу) |
| `ast.rs` — одна файл-область: параллельные фазы конфликтуют в git | средняя | Фазы Ф48–Ф52 **сериализуются** по файлу (или каждая — на своей ветке и мерж по одной); см. §8 |

---

## 8. Порядок работы, гейты и оценка

**Порядок:** Ф47 — обязательный предшественник (инфраструктура). Далее Ф48 → Ф49 → Ф50 → Ф51 → Ф52 в любом
порядке **по содержанию**, но последовательно **по `ast.rs`** (одна ветка/один мерж за раз); Ф53 — замыкающая.

**Гейт после каждой фазы (без него фаза не закрывается):**
1. `cargo test` (все) + `cargo clippy --all-targets` + `cargo fmt --check` — зелёные.
2. Скан одного живого проекта владельца — без падения, с осмысленным приростом узлов/рёбер.
3. `ob2h bench` — без регрессии recall (для фаз, добавляющих узлы в эмбеддинг).
4. Замер nodes/edges/времени — строка в `docs/PLAN_v1.7_bench.md`.
5. Коммит по §8 AGENTS.md (`feat(Ф48): ...`), чекбоксы в этом файле закрыты; отклонения — `PLAN.md §6`.

**Оценка:**

| Фаза | Дней |
|---|---|
| Ф47. Ядро: реестр, словари, тесты | 1–1.5 |
| Ф48. Веб: HTML/CSS/SFC | 1.5–2 |
| Ф49. API-контракты: proto/GraphQL/OpenAPI | 2 |
| Ф50. IaC: Terraform/Docker/K8s | 2 |
| Ф51. Манифесты и граф зависимостей | 1.5 |
| Ф52. Jupyter/Solidity/Zig | 1 |
| Ф53. Интеграция, замеры, релиз 1.7.0 | 1.5–2 |
| **Итого** | **10.5–12 фокусных дней** |

**DoD плана в целом:**
- [ ] Ф47–Ф52 закрыты, каждый новый формат имеет парсер, узлы/рёбра и тесты.
- [ ] Ф53: замеры (bench + скан трёх репозиториев) с числами, регрессия recall — ноль.
- [ ] Контракт MCP — 36 инструментов, снапшот `tools/list` совпадает, плагинный контракт-тест не изменён.
- [ ] Схема БД без миграции (проверено тестом `test_migration*`/`test_db`).
- [ ] `CHANGELOG.md` (v1.7.0), `docs/ARCHITECTURE.md`, `skills/ob2h/SKILL.md` + `skills/ob2h-zcode/SKILL.md`
      (синхронно), `PLAN.md §6` обновлены.
- [ ] `docs/PLAN_v1.7_bench.md` с таблицей до/после (nodes/edges/БД/p50/p95) и вердиктом по весам PPR.

---

## 9. Открытые вопросы к владельцу

1. **YAML-зависимость** (ADR-AST-3): допускаем ли новый крейт, или предпочитаем минимальный встроенный
   line-parser без зависимостей (медленнее писать, но нулевой риск по §6 AGENTS.md)?
2. **Объём значения:** нужны ли `Port`/`Volume`/`Network`/`Script`-узлы (Ф50/Ф51) или достаточно
   `Service`/`BaseImage`/`pkg:` — от этого зависит прирост числа узлов в БД.
3. **Веса PPR для кодовых рёбер** (47.4/53.3): менять `DEFAULT_PPR_WEIGHTS` по умолчанию (после замеров)
   или оставить opt-in через `OB2H_PPR_WEIGHTS`?
4. **Порядок важности форматов:** если делать не все семь фаз сразу — что нужнее: веб (Ф48, кейс
   presniakov.ru), IaC (Ф50, кейс Bitrix/Docker) или манифесты (Ф51, ускоряет impact-анализ)?
5. **Обратный порт в omnesagent-kag** — планировать сразу (как в прошлый раз) или отдельным решением?
