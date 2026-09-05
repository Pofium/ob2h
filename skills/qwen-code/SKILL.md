---
name: qwen-code
description: Qwen Code CLI (Alibaba) on this machine, version 0.23.0 — OB2H MCP via ~/.qwen/mcp.json. Use when configuring OB2H or MCP for Qwen Code.
---

# Qwen Code (Windows)

CLI-агент Alibaba. Один из 8 агентов из README OB2H.

## Установка / окружение
- Бинарник: `~/AppData/Roaming/npm/qwen(.cmd|.ps1)` (npm-установка).
- Версия: **0.23.0**.
- Домашняя папка: `~/.qwen/` (AGENTS.md, mcp.json, рабочие файлы).

## Интеграция OB2H
- MCP: `~/.qwen/mcp.json` → `mcpServers.ob2h` = `ob2h.exe serve`
  (`C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe`).
- Команда установки: `ob2h agent install --agent qwen`
- Статус: `ob2h agent status` (проверяет `~/.qwen/mcp.json`).

## Примечание
- `~/.qwen/AGENTS.md` — глобальные правила окружения Windows (ссылки на PHP,
  Node, Python, VPS-мост, gopass). Не удалять.
- Обычно оборачивается в mcp-compressor (см. devops/agent-mcp-setup).
