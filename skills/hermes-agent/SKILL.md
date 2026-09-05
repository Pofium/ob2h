---
name: hermes-agent
description: Hermes Agent — primary harness that OB2H runs in. Use when working with Hermes config, memory, skills, MCP setup, or the ob2h plugin/MemoryProvider integration.
---

# Hermes Agent (Windows) — хост OB2H

Hermes — главный «харнесс»: именно в нём живёт полноценная обвязка OB2H
(плагин памяти + MCP-сервер). Это машина: Windows 11, desktop-приложение.

## Установка / окружение
- Конфиг: `C:\Users\ipres\AppData\Local\hermes\config.yaml`
- Профиль: `default`; home = `~/AppData/Local/hermes/`
- Python (hermes venv): `~/AppData/Local/hermes/hermes-agent/venv/Scripts/python.exe`
- CLI (non-interactive): `hermes -z "промпт"`

## Интеграция OB2H (полная, целевой режим A — плагин)
- `memory.provider: ob2h` в config.yaml → каждый ход пишется автоматом,
  recall всплывает блоком `<agent_memory>` (индикатор 🧠).
- `mcp_servers.ob2h` тоже присутствует (режим B: плагин + MCP одновременно;
  работает, но два процесса — лучше уйти в чистый A, удалив MCP-запись).
- env-блок ob2h в config.yaml:
  - `OB2H_DATA_DIR: C:/Projects/omnesbot_for_hermes/data`
  - `OB2H_LLM_API_KEY: <РЕАЛЬНЫЙ ключ-литерал>` (Hermes НЕ пробрасывает
    .env-переменные MCP-подпроцессам — нельзя писать имя `DEEPSEEK_API_KEY`)
  - `OB2H_LLM_MODEL: deepseek-v4-flash`, `OB2H_EMBED_PROVIDER: local`
- Плагин: `~/AppData/Local/hermes/plugins/ob2h/` (spawnнит `ob2h serve`;
  до v1.0.0 падал с 401 — плагин сам резолвит OB2H_LLM_API_KEY конвенцией
  DEEPSEEK_API_KEY из .env/ос-окружения).

## Обслуживание
- `ob2h plugin status` — установлен ли плагин и включён ли в конфиге.
- `ob2h skill install` — деплой скилла в `$HERMES_HOME/skills/devops/ob2h/`.
- После смены env-блока ob2h — ТОЛЬКО полный рестарт Hermes (on-demand
  респавны используют кэш конфига на старте).
- Изменения конфига на Windows — править config.yaml напрямую
  (`hermes config set` на Windows зависает).

## Питфоллы
- `provider_models_cache.json` в `~/AppData/Local/hermes/` кэширует модельный
  список — удалять при жалобах на модели (перезапуск desktop после).
- Desktop-пикер моделей фильтрует по localStorage `hermes.desktop.visible-models`
  (Electron LevelDB) — сбрасывать если модель не появляется.
- После `hermes update` проверять proxy-env в юните hermes-gateway (VPS):
  апдейт стирает прокси → OpenRouter 403.
