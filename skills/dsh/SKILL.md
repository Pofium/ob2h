---
name: ob2h
description: "Долговременная память, AST-граф кода и дриминг OB2H, подключённые к DSH как MCP-сервер (инструменты mcp__ob2h__*). Загружай для recall о пользователе, его проектах и прошлых сессиях, для сохранения новых фактов, разбора кодовой базы и дриминга памяти."
---

# OB2H в DSH — память как MCP-сервер + cordis-плагин (режим 1)

## Как подключено
- Транспорт stdio: `{{BINARY}} serve`, cwd `{{PROJECT_DIR}}`, данные `{{DATA_DIR}}`
  (`ob2h.db`, `workspace/`, `logs/ob2h.log`).
- Регистрация — блок `mcp-ob2h` в patch-слое профиля DSH
  (`{{DSH_HOME}}/profiles/<профиль>/cordis.patch.yml`), плагин `@deepseek-ai/dsh-mcp-client`;
  инструменты видны как `mcp__ob2h__<имя>`.
- **Автозахват и prefetch идут сами**: cordis-плагин `{{PROJECT_DIR}}/plugin/dsh-ob2h`
  (строка `ob2h-memory` в том же блоке патча) слушает `session/event` и waterfall
  `agent/pre-step`. Релевантные воспоминания приходят первым сообщением хода в блоке
  `<agent_memory>…</agent_memory>` — их не нужно запрашивать инструментом, но и
  выдумывать поверх них нельзя. Ход диалога уходит в память при закрытии хода
  (`session_ingest`, источник `dsh`), ходы сабагентов — нет.
- Профиль с `patchReload: live` подхватывает патч без перезапуска; иначе нужен перезапуск DSH.

## Поведение
- Если блок `<agent_memory>` пришёл — опирайся на него; если его нет, а вопрос про
  пользователя/проекты/прошлые обсуждения — вызови recall (`memory_search` / `memory_context`).
- Отдельный `session_log` в конце хода не нужен (захват автоматический), но осмысленный
  вывод по-прежнему стоит фиксировать: устойчивый факт → `memory_save`
  (importance 0.8+, осмысленный `key`). «Запомни …» → `memory_save`.
- Запись проверять чтением (`memory_search` по ключу), а не по ответу «saved».
- Устаревший факт не удалять: `memory_update(key=...)` с полным новым текстом и пометкой
  `[УСТАРЕЛО <дата>: заменено — см. <новый ключ>]`.
- Документы/длинные тексты → `knowledge_extract`; связи «кто/что с чем связано» →
  `graph_reason`; дриминг → `dream_run` (фоновый AutoDreamWorker работает по гейтам сам).

## Кодовый граф (детерминированный, без LLM)
- `project_scan` + `project_scan_status`: AST-скан (Rust/Python/TS/Go/…, веб, IaC,
  манифесты). Скан идёт в фоне: ответ «still running» → поллинг статуса, не повторный запуск.
- **Всегда передавай `id` проекта явно** — без него сервер подставит «активный проект»
  своей сессии и вернёт 0 результатов.
- `project_context` (mode=repo_map) / `project_report` — God Nodes и архитектура;
  `project_call_path` — callers/callees/call-path; `project_impact` — blast radius правки.

## Питфоллы подключения
- **LLM-инструменты** (`dream_run`, `knowledge_extract`, `graph_reason`) требуют ключ:
  ob2h читает `OB2H_LLM_API_KEY` из окружения **или** из `.env` в рабочем каталоге
  (`{{PROJECT_DIR}}/.env`, он в `.gitignore`). В патч ключ не пишется: DSH вычищает из env
  ребёнка ambient-имена с KEY/TOKEN/SECRET, а пустая переменная мешала бы `dotenvy`
  подхватить `.env`. Значение может быть ИМЕНЕМ другой переменной с ключом
  (`OB2H_LLM_API_KEY=DEEPSEEK_API_KEY`) — код разворачивает индирекцию.
- **БД общая с Hermes** (`{{DATA_DIR}}/ob2h.db`): WAL + busy_timeout переживают два
  процесса, но `dream_run` из двух процессов одновременно не запускать.
- Read-only проверки: `sqlite3.connect("file:{{DATA_DIR}}/ob2h.db?mode=ro", uri=True)`,
  таблица `memories` (`key, content, importance, trust, deleted_at`).
- Логи: `{{DATA_DIR}}/logs/ob2h.log` (в stdio-режиме stderr не пишется).
- Переустановка идемпотентна: `ob2h agent install --agent dsh` из корня репо;
  статус — `ob2h agent status`.

## Синхронизация PC ↔ VPS
- Только папкой бандлов `{{DATA_DIR}}/sync/` (`ob2h sync push` / `pull`, `sync status`),
  никогда файловой синхронизацией живой `ob2h.db` (WAL = порча).
