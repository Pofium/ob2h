# Анализ AST-графа OmnesBot (ob2h v1.5.0)

Дата: 2026-10-05 · Инструмент: `ob2h 1.5.0` (Rust) · Проект: `omnesbot`
Корень: `/mnt/data2/omnes-aibot/OmnesBot` · База: `/mnt/data2/ob2h/data/ob2h.db`

---

## 1. Что сделано (настройка ob2h «под себя»)

| Шаг | Результат |
|---|---|
| Установка Rust | `rustup` → `cargo 1.99`, `rustc 1.99` |
| Системные зависимости | `pkg-config`, `libssl-dev` (нужны `openssl-sys` через `hf-hub`/`native-tls`) |
| Сборка | `cargo build --release` → `target/release/ob2h` (26 МБ), 2 warning'а |
| Модель эмбеддингов | `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2` (скачана в HF-кэш) |
| Проект | `ob2h project init --id omnesbot --path …/OmnesBOT` |
| Скан | `ob2h project scan --id omnesbot` |
| MCP для pi | добавлен сервер `ob2h` (`~/.pi/agent/mcp.json`, stdio, `--cwd /mnt/data2/ob2h`, `OB2H_DATA_DIR=/mnt/data2/ob2h/data`) → **36 инструментов подключены** |

---

## 2. Метрики графа (сырой AST-скан)

- Файлов: **1138**, строк: **208 199**
- Узлов: **9 621** → `Function` 6220, `ExternalModule` 1327, `Class` 1231, `File` 1138, `Interface` 109, `Route` 35, `Table` 12
- Рёбер: **19 068** → `DEFINES` 7567, `IMPORTS` 7622, `CALLS` ~1400, `INHERITS` 884, `ROUTE` 37
- Внутренних `File → File` IMPORTS после резолва (Ф41): **2 785**; внешних модулей осталось **481** (было 1327)
- Время скана: **1.9 с** (быстро, без LLM)
- God nodes (сырые) — почти все `ExternalModule`: `module:typing` (449), `logging` (239), `kag.interface` (232), `re` (223)…

> ⚠️ Сырой граф **непригоден** для анализа внутренних зависимостей — см. §3.
> Ниже в §4–5 используется граф с исправленным резолвом импортов (770 внутренних рёбер, 229 файлов-источников).

---

## 3. Дефекты самого ob2h (AST-экстрактор)

### 3.0 ✅ ИСПРАВЛЕНО (Ф41, 2026-10-05): резолв относительных и алиасных импортов

Добавлена функция `resolve_imports()` (`src/project/ast.rs`), вызывается в
`scan_project` сразу после `scan_directory`. Она нормализует имя модуля и
переписывает цель IMPORTS-ребра на `file:<rel_path>`, если модулю соответствует
файл проекта. Поддержано:
- относительные Python (`from .auth import …`, `from ..pkg.x import …`);
- абсолютные Python (`app.services.x`) и top-level пакеты из `sys.path` на
  `services/` (`kag`, `knext`, `omnesbot`);
- Vite/tsconfig-алиасы `@/…` → `frontend/src/renderer/`;
- TS/JS относительные `./x`, `../x`;
- нормализация `as`-алиасов и `import a, b` (Python): алиас отрезается,
  запятые разворачиваются в отдельные рёбра.

Проверено тестом `tests/test_typepass.rs::imports_resolved_to_internal_files`.
Эффект на графе OmnesBot: `ExternalModule` 1327 → 481, рёбер +873,
`backend/app/api/v1/auth.py` перестал быть «сиротой» (39 входящих импортов).

> ⚠️ Требуется **полный** рескан проекта (инкрементальный оставит старые
> `module:.x`-узлы в неизменённых файлах).

---

### 3.1 ✅ ИСПРАВЛЕНО (Ф43, 2026-10-06): многострочные сигнатуры, route-декораторы, `self.`-вызовы
- `py_fn_re` требовал `)` и `:` на той же строке, что и `def` → многострочные
  сигнатуры (напр. `ask_kag`) не попадали в граф. Теперь достаточно префикса
  `def name(`; `py_body_end` считает тело по отступам с учётом конца сигнатуры.
- route-декораторы склеиваются по строкам → многострочные `@router.post(\n "/x",\n)`
  больше не теряются (Route 35 → 272).
- `self.method()`/`cls.method()` резолвятся в метод того же файла (раньше любой
  `.`-receiver считался AMBIGUOUS). Проверено `test_python_multiline_signature_and_route`.

### 3.1 Прочие дефекты (остаются открытыми)

#### Относительные импорты Python (историческая справка)
До фикса: **144** уникальных `module:.x`, **471** ребро-импорт, `auth.py` был «сиротой».

### 3.2 ✅ Alias-импорты TS/Vite (`@/…`) — исправлено в Ф41 (298 рёбер).

### 3.3 ✅ Импорты с `as`-алиасами и через запятую — исправлено в Ф41.

### 3.4 ✅ ИСПРАВЛЕНО (Ф42, 2026-10-06): диапазон строк TS/TSX-функций/классов
Добавлена `ts_body_end()` (`src/project/ast.rs`): `line_end` считается по балансу
фигурных скобок вне строк/комментариев; expression-body стрелки (`=> v + 1`)
заканчиваются на `;`. `line_end` теперь учитывает `line_offset` (Vue/Svelte SFC).
Проверено `tests/test_ast_extractor.rs::test_ts_function_and_class_body_spans`.

### 3.5 ✅ ИСПРАВЛЕНО (Ф42, 2026-10-06): шум dead-code
- Исправлено направление исключения ROUTE/HANDLES в `dead_code` (`fe.source_id`,
  а не `target_id` — FastAPI-роуты раньше попадали в отчёт).
- `entrypoint_reason` расширен: пути (`alembic/`, `migrations/`, `conftest`),
  имена lifecycle/dunder (`upgrade`/`downgrade`/`setup`/`teardown`/`__init__`),
  Pydantic/dataclass-модели (по базе/декоратору в описании) и framework-декораторы
  (служебные `@property`/`@staticmethod`/… — не entrypoints).
Проверено `tests/test_callpath.rs::entrypoint_reason_covers_issue_35` и
`tests/test_track_c5.rs::dead_code_excludes_pydantic_decorated_and_migrations`.

---

## 4. 🏛️ Структура зависимостей OmnesBot (нативный вывод ob2h после Ф41)

### 4.1 Главные внутренние хабы (fan-in)
| Импортируют | Файл |
|---:|---|
| 234 | `backend/app/services/kag/interface/__init__.py` (vendored, hub) |
| 105 | `backend/app/services/knext/common/rest/configuration.py` (vendored) |
| 88 | `backend/app/services/kag/common/conf.py` (vendored) |
| 69 | `backend/app/db/postgres.py` |
| 56 | `backend/app/services/kag/common/utils.py` (vendored) |
| 55 | `backend/app/models/user.py` |
| 49 | `frontend/src/renderer/lib/utils.ts` |
| 39 | `backend/app/api/v1/auth.py` (`get_current_user` и Ко) |
| 36 | `backend/app/services/kag/builder/model/sub_graph.py` (vendored) |
| 29 | `frontend/src/renderer/services/api.ts` |
| 27 | `frontend/src/renderer/components/ui/button.tsx` |
| 24 | `backend/app/config.py` |

Изменения в этих файлах = максимальный blast radius.

### 4.2 Файлы с максимальным fan-out (связанность)
`api/v1/router.py` (42), `services/omnesbot/service.py` (**108** исходящих), `pages/ChatSSE.tsx` (25), `pages/Chat.tsx` (19), `services/omnesbot/tools.py` (19), `pages/Settings.tsx` (18).

### 4.3 Реальные циклы импортов (SCC > 1)
| Размер | Цикл |
|---|---|
| 5 | `kag/interface/{__init__,builder/reader_abc,common/llm_client,solver/base,solver/executor_abc}` (vendored) |
| 4 | `knext/schema/model/{spg_type,relation,property,base}` (vendored) |
| 3 | `services/omnesbot/__init__.py` ↔ `kag_dream.py` ↔ `service.py` |
| 3 | `api/v1/router.py` ↔ `main.py` ↔ `api/v1/telegram.py` |
| 2 | `core/runtime_config.py` ↔ `services/llm_config_service.py` (спасают lazy-import внутри функций) |
| 2 | `api/v1/auth.py` ↔ `api/v1/integrations_api.py` |
| 2 | `services/session_checkpoint_service.py` ↔ `services/session_persistence_v2.py` |
| 2 | `services/company_service.py` ↔ `services/user_service.py` |

---

## 5. 📌 Что нужно улучшить / изменить

### 5.1 Критично (архитектура)
1. **Vendored KAG/KNext = 60 % кодовой базы.** `services/kag` — 497 файлов, `services/knext` — 187 (итого **684 из 1138**), ~**3105 из 6220** функций. Runtime реально использует только `KAGReasoner` (`app.services.kag`), `kag_reasoning`, `kag_dream`, `kag_stream`. Остальное — копия OpenSPG/KAG (Apache-2.0, импортируется как пакет `kag.*`, а не `app.services.kag.*`).
   → Выпилить копию или оформить как отдельную зафиксированную зависимость; оставить прослойку `KAGReasoner`.
2. **`sys.path`-хак в `backend/app/main.py:10`.** `sys.path.insert(0, …/services)` даёт два способа импорта одного кода (`kag` vs `app.services.kag`, `omnesbot` vs `app.services.omnesbot`). Хрупко, ломает статический анализ и сборку.
   → Явный пакет + `install -e`/entry-points вместо манипуляции `sys.path`.
3. **God-файлы (God Nodes):**
   - `services/omnesbot/service.py` — 1844 строки, **108** исходящих связей;
   - `api/v1/extraction.py` — 1824; `services/omnesbot/tools.py` — 1654; `api/v1/chat_sse.py` — 956; `api/v1/omnesbot_admin.py` — 1237.
   → Разбить по ответственности (например, `service.py` → session/loop/tools/stream).

### 5.2 Важно
4. **Разорвать циклы** из §4.3 (ленивые импорты → выделить общий модуль/Protocol).
5. **Фронтенд:** `pages/ChatSSE.tsx` — 1490 строк, `AgentSettingsWidget.tsx` — 1240, `pages/Settings.tsx` — 979. Выделить хуки/подкомпоненты.
6. **Тесты:** 32 файла `test_*.py` на 6220 функций — покрытие API-роутов (35 `Route`) фактически отсутствует. Для `auth`, `permissions`, `tools` (деньги/безопасность) тестов нет.
7. **`TODO/FIXME/HACK/XXX`:** 30 вхождений в 13 файлах `backend/app` — разобрать.

### 5.3 Технический долг / мелочи
8. Дублирование имён-«близнецов»: `parse_response` (74 файла), `template_variables` (72), `invoke` (62) — в основном vendored, но проверить свои.
9. `permissions.py`, `db/neo4j.py`, `db/postgres.py` — в топе fan-in, покрыть тестами перед рефакторингом.
10. `backend/test_session_db.py` — тестовый файл в корне backend (не в `tests/`), попал в god-nodes — перенести в `tests/`.

---

## 6. Как воспроизвести

```bash
export OB2H_DATA_DIR=/mnt/data2/ob2h/data
cd /mnt/data2/ob2h
# после Ф41 нужен ОДИН полный рескан (инкрементальный оставит старые module:.x-узлы):
sqlite3 data/ob2h.db "DELETE FROM graph_edges WHERE project_id='omnesbot'; \
                      DELETE FROM graph_nodes WHERE project_id='omnesbot'; \
                      DELETE FROM project_files WHERE project_id='omnesbot';"
./target/release/ob2h project scan   --id omnesbot
./target/release/ob2h project report --id omnesbot
./target/release/ob2h project dead-code --id omnesbot
./target/release/ob2h project repo-map  --id omnesbot --tokens 4096
```

Сырые артефакты: `/tmp/omnesbot_report.md`, `/tmp/omnesbot_deadcode.txt`, `/tmp/omnesbot_repomap.txt`.

---

## 7. ✅ Обновление после Ф43 (2026-10-06): полный рескан и итоги

Реализованы задачи work-list для ob2h (см. `../issues/WORK_LIST_ISSUES.md` в
`omnes-aibot`):

- **Ф43.1** — многострочные `def`/`async def` (см. §3.1) и `py_body_end`.
- **Ф43.2** — многострочные route-декораторы FastAPI/Flask.
- **Ф43.3** — резолв `self.`/`cls.`-вызовов по классу в том же файле.
- **Ф43.4** — пропуск сгенерированного protobuf (`*_pb2.py`, `*_pb2_grpc.py`) в
  `collect_files` (устранён шум `arks_pb2.py` — 2329 строк).
- **Ф43.5** — `ob2h project scan --id <id> --full` (incremental=false) и
  `AST_EXTRACTOR_VERSION` в `kv` на проект: при смене версии экстрактора скан
  автоматически становится полным (проверено `tests/test_scan_full.rs`).

### Метрики графа OmnesBot (до → после, полный рескан)

| Метрика | До | После |
|---|---:|---:|
| Файлов | 1138 | 1133 |
| Строк | 208 199 | 204 007 |
| Узлов | 9 621 | 10 454 |
| Функций | 6 220 | 7 229 |
| Route-узлов | 35 | 272 |
| ROUTE-рёбер | 37 | 349 |
| CALLS (RESOLVED), рёбер | ~1 400 | ~4 984 |
| Мёртвых кандидатов | 4 656 | 3 564 |

### Статус пунктов §5

- §5.1.1 (вендорные kag/knext/omnesbot) — ⏸ отложено (крупная упаковка).
- §5.1.2 (`sys.path`-хак) — ⏸ отложено (следует из §5.1.1).
- §5.1.3 (god-файлы) — ⏸ отложено (механический рефакторинг отдельной веткой).
- §5.2.4 (циклы импортов) — ⏸ отложено; вендорные + собственные циклы остаются.
- §5.2.7 (TODO/FIXME/HACK) — ✅ 3 реальных TODO разобраны (лимит графов компании,
  доступ GROUP_ADMIN, LLM-декомпозиция); остальные 27 — шаблонные `XXX` в vendored
  skill-creator.
- §5.3.10 (`test_session_db.py`) — ✅ перенесён в `backend/tests/session_db_check.py`.
- Плюс удалён мёртвый код: **61** символ backend и **15** — frontend/TS.
