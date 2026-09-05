---
name: windsurf
description: Windsurf / Cascade IDE on this machine — OB2H MCP via ~/.codeium/windsurf/mcp_config.json. Use when configuring OB2H or MCP for Windsurf.
---

# Windsurf / Cascade (Windows)

IDE (Codeium). Один из 8 агентов/IDE из README OB2H.

## Установка / окружение
- Конфиги: `~/.codeium/windsurf/` (mcp_config.json).
- CLI-бинарник `windsurf` в PATH отсутствует.

## Интеграция OB2H
- MCP: `~/.codeium/windsurf/mcp_config.json` → `mcpServers.ob2h` = `ob2h.exe serve`
  (путь exe: `C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe`).
- Рядом уже прописан и `puppeteer` (npx `@modelcontextprotocol/server-puppeteer`).
- Команда установки: `ob2h agent install --agent windsurf`
- Статус: `ob2h agent status` (проверяет mcp_config.json).
