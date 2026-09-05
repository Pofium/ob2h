---
name: codex
description: Codex CLI (OpenAI) on this machine — config at ~/.codex/config.toml (TOML, literal strings for Windows paths). Use when configuring OB2H or MCP servers for Codex.
---

# Codex CLI (Windows)

CLI-агент OpenAI. На этом компе есть конфиг `~/.codex/config.toml`
(бинарник `codex` на PATH сейчас не найден — конфиг готов к подключению).

## Окружение
- Конфиг: `~/.codex/config.toml` (TOML; **Windows-пути — в literal-строках '...'**,
  бэкап `config.toml.bak-mcc`).
- Формат MCP — секции `[mcp_servers.<name>]` с `command`/`args`/`env`.
- Все MCP-команды оборачиваются в `mcp-compressor -c medium --`.
- Текущие MCP: playwright, chrome-devtools, searxng (SEARXNG_URL=searx.presniakov.ru),
  windows-mcp, magicui, figma (FIGMA_API_KEY).
- ⚠️ примечание из самого конфига: sqz НЕ предоставляет MCP-сервер (регистрация
  `sqz-mcp` ломала Codex ошибкой -32000) — ob2h это НЕ касается, но не добавлять
  мнимые бинарники.

## Интеграция OB2H
- ⚠️ **НЕ подключён**: ob2h в config.toml отсутствует.
  Для подключения добавить секцию (literal-строка пути):
  ```toml
  [mcp_servers.ob2h]
  command = 'C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe'
  args = ['serve']

  [mcp_servers.ob2h.env]
  OB2H_DATA_DIR = 'C:/Projects/omnesbot_for_hermes/data'
  ```
- После правки — перезапустить Codex.
