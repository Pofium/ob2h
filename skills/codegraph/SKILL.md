---
name: codegraph
description: CodeGraph (codegraph CLI 1.5.0) on this machine — repo indexing + MCP, complementary to OB2H AST-graph. Use when choosing/tuning code-intelligence tools alongside OB2H.
---

# CodeGraph (Windows)

Индексатор/анализатор кода (colbymchenry). Присутствует на этом компе.
Работает ПАРАЛЛЕЛЬНО с OB2H AST-графом — оба дают понимание кодовой базы.

## Окружение
- Бинарник: `~/AppData/Roaming/npm/codegraph` (npm). Версия: **1.5.0**.
- Данные: `~/.codegraph/` (daemons, telemetry, update-check, beta-signup).
- В индексируемых репо — каталог `.codegraph/` в корне.
- MCP: `codegraph serve --mcp`.

## Интеграция OB2H
- Не заменяет OB2H: CodeGraph — семантика кода по дереву-ситтеру,
  OB2H (`project_scan`) — детерминированный AST-граф (классы/функции/трейты/связи)
  + память и дриминг. Использовать:
  - CodeGraph `codegraph_explore` / `codegraph explore "<символ или вопрос>"` —
    БЫСТРОЕ чтение кода по именам и путям вызовов (ПЕРЕД grep/find).
  - OB2H `project_*` — архитектурный дайджест, God Nodes, blast radius, память.
- Питфолл: если в репо НЕТ `.codegraph/` — индексация не делалась, CodeGraph
  не спрашивать (индексация — решение пользователя).

## Ссылки
- Скилл: software-development / codebase-inspection, devops/agent-mcp-setup
  (MCP-конфиги всех агентов содержат codegraph через mcp-compressor).
