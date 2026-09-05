---
name: mimo-code
description: MiMo Code CLI (Xiaomi) on this machine, version 0.1.12 — MCP/agent setup, ponytail skills, connect. Use when developing or configuring MiMo/Codex-style agent setups.
---

# MiMo Code (Windows)

CLI-агент Xiaomi. Присутствует на этом компе.

## Окружение
- Бинарник: `~/AppData/Roaming/npm/mimo` (npm). Версия: **0.1.12**.
- Конфиг/workspace: `~/.mimocode/` (skills в `.mimocode/skills/`).
- Подключение: `/connect` или env `MIMO_API_KEY`.
- Агенты: Build (полный), Plan (read-only), General/Explore (субагенты).
- Docs: https://mimo.xiaomi.com/zh/mimocode/start

## Скиллы
- Скиллы совместимы с Hermes: SKILL.md в `.mimocode/skills/`.
- Ponytail-режим (ленивый сеньор) — зашит в AGENTS.md, есть `/ponytail [lite|full|ultra|off]`.

## Интеграция OB2H
- ⚠️ ob2h в конфиг MiMo не подключён.
- Для подключения добавить MCP-сервер ob2h (аналогично другим агентам):
  `ob2h.exe serve` с `OB2H_DATA_DIR=C:/Projects/omnesbot_for_hermes/data`.
