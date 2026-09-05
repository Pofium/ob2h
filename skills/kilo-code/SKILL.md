---
name: kilo-code
description: Kilo Code (VS Code extension) on this machine — MCP via globalStorage/kilocode.kilo-code/settings/mcp_settings.json. Use when configuring OB2H or MCP for Kilo Code.
---

# Kilo Code (Windows)

AI-расширение для VS Code (Kilo). Присутствует на этом компе.

## Окружение
- Конфиг MCP: `~/AppData/Roaming/Code/User/globalStorage/kilocode.kilo-code/settings/mcp_settings.json`
- Обычный `mcpServers` + расширенные поля (`alwaysAllow` для автоодобрения).
- MCP-команды оборачиваются в `mcp-compressor -c medium --`.

## Текущие MCP (эта машина)
- context7, ddg-search (duckduckgo, `alwaysAllow: ["search"]`), Puppeteer и др.

## Интеграция OB2H
- ⚠️ **НЕ подключён**: ob2h в mcp_settings.json отсутствует.
  Для подключения добавить в `mcpServers`:
  ```json
  "ob2h": {
    "command": "C:\\Projects\\omnesbot_for_hermes\\target\\release\\ob2h.exe",
    "args": ["serve"],
    "env": { "OB2H_DATA_DIR": "C:/Projects/omnesbot_for_hermes/data" },
    "alwaysAllow": []
  }
  ```
- После правки — перезагрузить VS Code / Kilo Code.
