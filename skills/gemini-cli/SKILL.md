---
name: gemini-cli
description: Gemini CLI / Antigravity IDE on this machine — OB2H MCP registered in both ~/.gemini/antigravity-ide and ~/.gemini/antigravity. Use when configuring OB2H or MCP for Gemini/Antigravity.
---

# Gemini CLI / Antigravity (Windows)

CLI-агент Google + Antigravity IDE. Один из 8 агентов/IDE из README OB2H.

## Установка / окружение
- CLI бинарник: `~/AppData/Roaming/npm/gemini` (npm-установка).
- Версия: **0.55.1**.
- Домашняя папка: `~/.gemini/` — содержит `antigravity/`, `antigravity-ide/`,
  `config/`, settings.json, oauth_creds.json, google_accounts.json.

## Интеграция OB2H
- MCP-конфиги пишутся в ДВА места (оба существуют):
  1. `~/.gemini/antigravity-ide/mcp_config.json` — ob2h зарегистрирован
  2. `~/.gemini/antigravity/mcp_config.json` — ob2h зарегистрирован
  Оба: `mcpServers.ob2h` = `ob2h.exe serve`
  (`C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe`).
- Рядом в этих конфигах уже есть chrome-devtools, cloudrun, codegraph,
  dart-mcp-server, figma и др.
- Команда установки: `ob2h agent install --agent gemini`
- Статус: `ob2h agent status` (проверяет antigravity-ide/mcp_config.json).
