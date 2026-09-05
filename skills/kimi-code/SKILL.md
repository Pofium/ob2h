---
name: kimi-code
description: Kimi Code CLI (Moonshot) on this machine, version 0.36.1 — MCP via ~/.kimi-code/mcp.json. Use when configuring OB2H or MCP for Kimi.
---

# Kimi Code (Windows)

CLI-агент Moonshot. Присутствует на этом компе.

## Окружение
- Обёртка: `~/bin/kimi` (sqz dedup :9999 | hooks активны). Версия: **0.36.1**.
- Конфиг MCP: `~/.kimi-code/mcp.json` (классический `mcpServers` + `enabled`).
- Домашняя папка: `~/.kimi-code/` (mcp.json, AGENTS.md).

## Текущие MCP (эта машина)
- chrome-devtools (CHROME_REMOTE_URL=http://localhost:9222), codegraph и др.
  — все через `mcp-compressor -c medium --`.

## Интеграция OB2H
- ⚠️ **НЕ подключён**: ob2h в mcp.json отсутствует.
  Для подключения добавить в `mcpServers`:
  ```json
  "ob2h": {
    "command": "C:\\Projects\\omnesbot_for_hermes\\target\\release\\ob2h.exe",
    "args": ["serve"],
    "env": { "OB2H_DATA_DIR": "C:/Projects/omnesbot_for_hermes/data" },
    "enabled": true
  }
  ```
- После правки — перезапустить Kimi.
