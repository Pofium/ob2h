---
name: trae
description: Trae IDE (ByteDance) on this machine — MCP via ~/AppData/Roaming/Trae/User/mcp.json. Use when configuring OB2H or any MCP server for Trae.
---

# Trae (Windows)

IDE от ByteDance. Присутствует на этом компе (конфиг-папка `~/AppData/Roaming/Trae`).

## Окружение
- Конфиг MCP: `~/AppData/Roaming/Trae/User/mcp.json`
- Общий формат — `mcpServers`, команды оборачиваются в `mcp-compressor -c medium --`.
- В Trae/Figma плагины определённого типа несут поле `fromGalleryId`
  (напр. `GLips.Figma-Context-MCP` для «Figma AI Bridge»).

## Текущие MCP (на этой машине)
- `Figma AI Bridge` (figma-developer-mcp, ключ FIGMA_API_KEY)
- `GitHub` (@modelcontextprotocol/server-github, GITHUB_PERSONAL_ACCESS_TOKEN)
- и др. (context7, puppeteer и т.п. — все через mcp-compressor)

## Интеграция OB2H
- ✅ **Подключён** (2026-11-02):
  ```json
  "ob2h": {
    "command": "C:\\Projects\\ob2h\\target\\release\\ob2h.exe",
    "args": ["serve"],
    "env": { "OB2H_DATA_DIR": "C:/Projects/ob2h/data" }
  }
  ```
- Smoke-тест (initialize → tools/list): OK, 36 tools.
- После правки — перезапустить Trae.
