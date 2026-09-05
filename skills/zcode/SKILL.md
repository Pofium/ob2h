---
name: zcode
description: ZCode IDE (Z.ai, GLM) on this machine — has its own skills dir and OB2H MCP pointing at a different data dir. Use when developing/configuring OB2H for ZCode.
---

# ZCode (Windows)

Electron IDE от Z.ai (GLM). Один из 8 агентов/IDE из README OB2H.

## Установка / окружение
- Приложение: `C:\Users\ipres\AppData\Local\Programs\ZCode\ZCode.exe`
- Директория конфигов/workspace: `~/.zcode/` — содержит `cli/`, `mcp.json`,
  `plugin-workspace/`, `skills/`, `v2/`, `workspace/`.
- Обёртка CLI: `~/bin/zcode` (только sqz dedup; API несовместим с прокси).

## Интеграция OB2H
- MCP: `~/.zcode/mcp.json` и `~/.zcode/cli/config.json` → `mcp.servers.ob2h`,
  команда `ob2h.exe serve`.
- ⚠️ С 05.09.2026 ZCode использует ОБЩУЮ память всех агентов:
  - exe: `C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe`
  - `env.OB2H_DATA_DIR: C:\Projects\omnesbot_for_hermes\data` (единая БД)
  - `cwd: C:\Projects\Omnes-agent` (рабочий проект ZCode, НЕ хранилище памяти)
  → Отдельный инстанс памяти `C:\Projects\Omnes-agent\data` был слит в основную
  БД (sync-импорт, origin=omnes) и удалён 05.09.2026. В БД факты с origin=omnes.
- Skills: `~/.zcode/skills/ob2h/SKILL.md` (свой формат, `$typeName` для плагинов).
- Команда установки: `ob2h agent install --agent zcode` (+ `--path` для кастомного).
- Статус: `ob2h agent status` (проверяет `~/.zcode/mcp.json`).

## Примечание
- Формат конфига ZCode — свой (`$typeName`), не оборачивать в mcp-compressor
  как у других агентов, если там не предусмотрено.
