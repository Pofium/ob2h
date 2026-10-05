# Черновики GitHub Issues для `Pofium/ob2h`

Составлено по результатам AST-анализа OmnesBot (v1.5.0, `Ф41`).
Создать разом: см. §«Как завести» в конце файла.

---

## Issue 1 — `dead_code` не учитывает точки входа → ~90 % ложных срабатываний

**Labels:** `bug`, `ast`, `analytics`
**Title:** `project dead-code`: не учитываются точки входа (декораторы, Pydantic, тесты, alembic)

### Что происходит
На реальном проекте (OmnesBot, 1138 файлов) `ob2h project dead-code --id omnesbot` и раздел «Кандидаты в мёртвый код» отчёта дают **5606 из 6220 функций (90 %)**. В списке — заведомо живые символы.

Примеры из вывода:
```
backend/alembic/env.py:52 `do_run_migrations` [Function]
backend/alembic/versions/002_groups_and_defaults.py:22 `upgrade` [Function]
backend/app/api/v1/agent_settings.py:431 `McpServerEntry` [Class]
backend/app/api/v1/auth.py:178 `logout` [Function]
backend/app/api/v1/chat.py:33 `ChatRequest` [Class]
```

### Почему это важно
Метрика бесполезна на практике и подрывает доверие ко всему анализу (её же выводит `project report`).

### Причина
`graph::callpath::dead_code` считает «мёртвым» узел с in-degree 0 по `CALLS/IMPORTS/IMPLEMENTS/DEPENDS_ON`. Не учитываются:
- FastAPI/Flask-роуты (`@router.get/post/...`), Django-вьюхи;
- Pydantic-модели (`BaseModel`) и dataclass-схемы — используются через сериализацию, а не вызов;
- `upgrade`/`downgrade` в `alembic/versions/**`, `migrations/**`;
- `tests/**`, `conftest.py`, `@pytest.fixture`;
- `if __name__ == "__main__"`, `__all__`, entry_points/плагины, CLI-команды clap;
- символы, экспортируемые из `__init__.py` (`from .x import Y as Y`).

### Ожидаемое поведение
- Точки входа (по путям + декораторам + маркерам) исключаются из кандидатов **до** подсчёта, а не после;
- либо выводятся отдельной секцией «entrypoints (не мертвы)»;
- в шапке — честная база: `N кандидатов из M функций (исключено K entrypoints)`.

### Как воспроизвести
```bash
ob2h project init --id demo --name Demo --path <repo>
ob2h project scan --id demo
ob2h project dead-code --id demo | head
```

### Возможное решение (минимальное)
Фильтр в `dead_code`/`format_dead_code`: пропускать, если
`file_path` матчит `tests/`, `conftest.py`, `alembic/versions/`, `migrations/`, `*.spec.*`, `*.test.*`;
или узел имеет `-декоратор` в `context` DEFINES-ребра (`router.`, `app.`, `pytest.`, `fixture`, `task`, `cli`);
или имя ∈ `{upgrade, downgrade, main, setup, teardown, teardown_module, __init__}`.

---

## Issue 2 — TS/TSX: `line_end == line_start`, у функций нет диапазона тела

**Labels:** `bug`, `ast`
**Title:** JS/TS/TSX-функции не имеют диапазона строк (span всегда 0)

### Что происходит
У всех функций/классов в `.ts/.tsx/.js/.jsx` `line_start == line_end` (только строка объявления). Проверка:
```bash
sqlite3 data/ob2h.db "SELECT label,file_path,(line_end-line_start) s FROM graph_nodes
  WHERE project_id='omnesbot' AND node_type='Function' ORDER BY s DESC LIMIT 5;"
# → все span = 0
```
Для Python/Rust `line_end` тоже часто только объявление — но там хотя бы для части символов есть тело.

### Почему это важно
- `project_impact`/blast radius по фронтенду считает по одной строке — режет не то;
- repo-map и архитектурный дайджест не показывают размеров;
- dead-code/PPR-сиды по строкам некорректны.

### Ожидаемое поведение
`line_end` = номер строки закрывающей скобки тела (с учётом вложенности/отступов), как уже делается для Python/Rust (если делается).

### Возможное решение
Для JS/TS: после объявления идти по строкам, отслеживая баланс `{}`/`()`, до закрытия тела; либо по отступу (строка с меньшим отступом, чем объявление). Тест: функция на 40 строк → `line_end - line_start ≈ 39`.

---

## Issue 3 — Инкрементальный скан оставляет устаревшие узлы после изменения логики экстрактора

**Labels:** `bug`, `ast`, `dx`
**Title:** После изменения AST-экстрактора инкрементальный скан не инвалидирует неизменённые файлы

### Что происходит
`project scan` всегда инкрементальный (`scan_project(id, path, true)`): пересканируются только файлы с изменившимся `sha256`. Если изменилась **логика экстрактора** (например, `Ф41` — резолв относительно/алиасных импортов), то у неизменённых файлов в БД остаются старые рёбра и узлы.

После `Ф41` это даёт смешанный граф: у изменённых файлов `IMPORTS → file:<rel>`, у неизменённых — старые `IMPORTS → module:.auth`. Пока лечится только ручным `DELETE FROM graph_nodes WHERE project_id=...`.

### Ожидаемое поведение
- Версия схемы/экстрактора хранится рядом с проектом (или в `kv`/`projects`);
- при несовпадении версии `project scan` автоматически делает полный рескан (или печатает явное предупреждение «нужен полный рескан, запустите `project scan --full`»).

### Возможное решение
Константа `AST_EXTRACTOR_VERSION` в `src/project/ast.rs`; колонка `ast_version` в `project_files`/`projects`; сравнение при скане.

---

## Issue 4 — CLI: нет `project scan --full` (форсировать полный рескан)

**Labels:** `enhancement`, `cli`, `dx`
**Title:** Добавить флаг `--full` у `ob2h project scan`

### Что происходит
`main.rs` жёстко передаёт `incremental=true`. Форсировать полный рескан можно только правкой БД вручную (`DELETE FROM graph_nodes/project_files WHERE project_id=...`). Нужно для:
- смены логики экстрактора (см. Issue 3);
- восстановления после повреждённого/частичного графа;
- первого добавления проекта без предварительного `project init` в правильном состоянии.

### Ожидаемое поведение
```bash
ob2h project scan --id omnesbot --full   # полный рескан, старые ast-узлы очищаются
```
`--full` прокидывается в `scan_project(id, path, false)`.

---

## Issue 5 (low) — Резолв не-кодовых ассетов в TS не делается намеренно, но засоряет граф

**Labels:** `enhancement`, `ast`
**Title:** Импорты ассетов (`./x.css`, `@/assets/logo.svg`, `./en.json`) остаются узлами-внешними модулями

### Что происходит
После `Ф41` резолвятся только кодовые расширения (`py`, `ts`, `tsx`, `js`, `jsx`). Импорты ассетов остаются как
```
module:@/assets/logo.svg
module:./ru.json
module:./index.css
```
Они безвредны, но попадают в `ExternalModule` и в статистику зависимостей.

### Ожидаемое поведение
Либо считать ассеты ресурсами и не создавать узел `module:*` (или помечать `node_type=Asset`), либо оставить как есть и задокументировать фильтр в аналитике. Не срочно.

---

## Как завести

Тексты готовы: у каждого issue выше есть `Title`, `Labels` и тело. Нужен токен
со scope `repo` (или `gh auth login`).

Вариант с `gh` (по одному issue на файл — вынести тело в `docs/issues/NN-*.md`):
```bash
gh issue create -R Pofium/ob2h \
  --title "project dead-code: не учитываются точки входа" \
  --label bug,ast,analytics \
  --body-file docs/issues/01-dead-code-entrypoints.md
```

Вариант без `gh`, через API (пример Issue 1):
```bash
TOKEN=...
curl -sS -X POST \
  -H "Authorization: Bearer $TOKEN" \
  -H "Accept: application/vnd.github+json" \
  https://api.github.com/repos/Pofium/ob2h/issues \
  -d @- <<'JSON'
{"title":"project dead-code: не учитываются точки входа",
 "labels":["bug","ast","analytics"],
 "body":"<markdown из Issue 1>"}
JSON
```

> Проверь перед созданием: репозиторий `Pofium/ob2h` — не твой, на запись/создание
> issue нужны права. Если их нет — заводи в своём форке и линкуй сюда.
