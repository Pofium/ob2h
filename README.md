# OmnesMemory — локальное хранилище знаний для Hermes (MCP)

Персональная локальная версия системы знаний корпоративного агента **OmnesBOT**
(`C:\Projects\omnes-aibot`), упакованная в виде **MCP-сервера** для личного агента
**Hermes** (Nous Research, конфиг: `C:\Users\ipres\AppData\Local\hermes\config.yaml`).

## Что это

Один Windows-процесс, который Hermes запускает через stdio. Внутри — всё, чем
хорош OmnesBOT в части знаний, но без Postgres, Neo4j, Docker и мультипользовательности:

| Возможность | Что переносится из OmnesBOT |
|---|---|
| Память (факты, важность, гибридный поиск) | `MemoryService` — FTS + вектора, слияние RRF |
| Рабочие файлы агента (MEMORY.md / SOUL.md / USER.md / history.jsonl) | `MemoryStore` + `GitStore` (git-история изменений) |
| Консолидация сессий по бюджету токенов | `Consolidator` |
| Граф знаний (сущности/отношения, извлечение из текста) | OneKE-пайплайн экстракции + `KAGReasoningService` (PG-путь) |
| KAG-рассуждение (ответ по графу с уверенностью) | `graph_reason` |
| **Дриминг** — фоновая консолидация памяти «во сне» | `Dream` (2 фазы) + `AutoDreamWorker` (гейты) |

## Принципы

1. **Только локально, один пользователь.** SQLite (WAL) + файлы + git. Никаких серверов БД.
2. **Ноль тяжёлых зависимостей.** Без torch/transformers: эмбеддинги через `fastembed` (ONNX, CPU)
   или OpenAI-совместимый API. Вектора — BLOB + numpy (личный масштаб, до ~50k записей).
3. **LLM через OpenAI-совместимый API** (deepseek / openrouter / aitunnel — ключи уже есть у Hermes).
4. **Портировать, а не изобретать**: проверенные идеи берём из omnes-aibot (карта источников —
   `docs/REFERENCE_omnesbot.md`), выкидывая мультиарендность, RBAC и vendored KAG/OpenSPG.

## Документы

- [`PLAN.md`](PLAN.md) — **подробный план разработки** (фазы, задачи, критерии готовности, риски)
- [`AGENTS.md`](AGENTS.md) — правила проекта для агентов (обязательно к прочтению перед работой)
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — целевая архитектура и схема хранения
- [`docs/REFERENCE_omnesbot.md`](docs/REFERENCE_omnesbot.md) — карта переноса компонентов из omnes-aibot
- [`docs/HERMES_INTEGRATION.md`](docs/HERMES_INTEGRATION.md) — как подключить сервер к Hermes

## Статус

**v0.1.0 — реализованы все фазы 1–6** (план: `PLAN.md`). 102 теста зелёные.
Осталось вручную: подключить сервер к живому Hermes (сниппет —
`docs/HERMES_INTEGRATION.md`) и прогнать живой e2e-чеклист (PLAN.md §6.4).

## Быстрый старт

```bash
cd C:\Projects\omnesbot_for_hermes
python -m venv .venv
.venv\Scripts\pip install -e ".[local,docs,dev]"
.venv\Scripts\pytest                      # проверка окружения
# эмбеддинг-модель (0.22 ГБ) уже скачана в ~/.cache/fastembed при первой установке
```

Запуск вручную (для отладки; Hermes сам поднимает сервер через stdio):

```bash
# MCP-сервер (молча ждёт stdio — это норма):
.venv\Scripts\python -m omnes_memory.server

# CLI: дрим/статус/бэкап без Hermes:
.venv\Scripts\python -m omnes_memory.dream_cli run
.venv\Scripts\python -m omnes_memory.dream_cli status
.venv\Scripts\python -m omnes_memory.dream_cli backup
```

Переменные окружения (все с дефолтами): `OMNES_DATA_DIR`, `OMNES_LLM_BASE_URL`,
`OMNES_LLM_API_KEY`, `OMNES_LLM_MODEL`, `OMNES_EMBED_PROVIDER` (`local|api`),
`OMNES_EMBED_MODEL`, `OMNES_AUTODREAM_ENABLED` — полный список
`docs/ARCHITECTURE.md` §6.
