---
name: qoder
description: Qoder IDE + qodercli on this machine — MCP via ~/.qoder/mcp.json. Use when configuring OB2H or MCP servers for Qoder.
---

# Qoder (Windows)

IDE + CLI лекарство от Tempo (присутствует на компе).

## Окружение
- IDE: `~/AppData/Local/Programs/Qoder IDE/`
- CLI: `qodercli` (npm/дистрибутив) — версия **1.1.23**
- Конфиг MCP: `~/.qoder/mcp.json` (классический `mcpServers`)
- Все команды MCP оборачиваются в `mcp-compressor -c medium --`.
- Текущие MCP здесь: context7, ddg-search, Puppeteer и др.

## Интеграция OB2H
- ✅ **Подключён** (2026-11-01):
  ```json
  "ob2h": {
    "command": "C:\\Projects\\ob2h\\target\\release\\ob2h.exe",
    "args": ["serve"],
    "env": { "OB2H_DATA_DIR": "C:/Projects/ob2h/data" }
  }
  ```
- Smoke-тест (initialize → tools/list): OK, 36 tools.
- Qoder — полноценный агентный IDE: OB2H даёт память,
  AST-граф кода и дриминг прямо в IDE.
