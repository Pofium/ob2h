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
- ⚠️ **НЕ подключён**: ob2h в mcp.json отсутствует.
  Для подключения добавить в `mcpServers`:
  ```json
  "ob2h": {
    "command": "C:\\Projects\\omnesbot_for_hermes\\target\\release\\ob2h.exe",
    "args": ["serve"],
    "env": { "OB2H_DATA_DIR": "C:/Projects/omnesbot_for_hermes/data" }
  }
  ```
- Qoder — полноценный агентный IDE: после подключения OB2H даст память,
  AST-граф кода и дриминг прямо в IDE.
