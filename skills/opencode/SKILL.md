---
name: opencode
description: OpenCode CLI on this machine, version 1.18.18 — OB2H MCP NOT yet wired into ~/.config/opencode/opencode.json. Use when configuring OB2H or MCP for OpenCode.
---

# OpenCode (Windows)

CLI-агент (opencode.ai). Один из 8 агентов из README OB2H.

## Установка / окружение
- Бинарник: `~/AppData/Roaming/npm/opencode(.cmd|.ps1)` (npm-установка).
- Версия: **1.18.18**.
- Конфиг: `~/.config/opencode/opencode.json` (JSON с `$schema`, `plugin`, `mcp`).
  Доп. папка: `~/.opencode/`.

## Интеграция OB2H
- ⚠️ **ПОКА НЕ ПОДКЛЮЧЕН**: в текущем `opencode.json` сервера `ob2h` НЕТ
  (есть zai-mcp-server, web-search-prime, web-reader, zread и др.).
  Для подключения добавить в `mcp` блок:
  ```json
  "ob2h": {
    "type": "local",
    "command": ["C:\\Projects\\omnesbot_for_hermes\\target\\release\\ob2h.exe", "serve"],
    "environment": {}
  }
  ```
- Команда установки: `ob2h agent install --agent opencode`
  → пишет `~/.opencode/...` / `~/.config/opencode/` (см. status ниже).
- Статус: `ob2h agent status` проверяет `~/.opencode/mcp.json`
  (файл `~/.opencode/` — проверять: реальный конфиг OpenCode лежит в
  `~/.config/opencode/opencode.json`; согласовать paths при несоответствии).

## Примечание
- Формат OpenCode: `mcp.<name>.type = local` + `command` массивом + `environment`.
  Не путать с классическим `mcpServers`.
