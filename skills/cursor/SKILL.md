---
name: cursor
description: Cursor IDE (VS Code fork) on this machine — MCP wiring for OB2H via ~/.cursor/mcp.json. Use when configuring OB2H or MCP servers for Cursor.
---

# Cursor (Windows)

IDE на базе VS Code. Один из 8 агентов/IDE из README OB2H.

## Установка / окружение
- Клиент: desktop-приложение (не CLI). Директория конфигов: `~/.cursor/`
  (mcp.json, проектные settings).
- CLI-бинарник `cursor` в PATH отсутствует — интеграция только через MCP-конфиг.

## Интеграция OB2H
- MCP: `~/.cursor/mcp.json` → `mcpServers.ob2h` = `ob2h.exe serve`
  (путь exe: `C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe`).
- Команда установки: `ob2h agent install --agent cursor`
- Кастомный путь: `ob2h agent install --agent cursor --path <dir>`
  → пишет `<dir>/.cursor/mcp.json`.
- Статус: `ob2h agent status` (проверяет `~/.cursor/mcp.json`).

## Примечание
- Cursor видит OB2H как обычный local MCP-сервер (`ob2h serve`, stdio).
  После правки mcp.json перезапустить/перезагрузить Cursor.
