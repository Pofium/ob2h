---
name: deepx
description: DeepX CLI (Go-native coding agent) on this machine, version 0.2.91. Wrapper ~/bin/deepx uses sqz only. Use when developing or configuring DeepX agent setups.
---

# DeepX (Windows)

Go-native coding-агент. Присутствует на этом компе.

## Окружение
- Бинарник: `~/bin/deepx.exe` (восстановлен из ~/deepx.zip, авг 2026).
  Обёртки: `~/bin/deepx`, `~/bin/deepx.bat`.
- Версия: **0.2.91** (commit 77005d8..., built 2026-07-07).
- Особенность: `[deepx] prompt-cache ~99%` — хороший кэш промптов.

## Интеграция OB2H
- ⚠️ ob2h в конфиг DeepX не подключён (wrapper — только sqz dedup, API-прокси
  несовместим).
- Для подключения MCP-сервер ob2h: `ob2h.exe serve` с корректным OB2H_DATA_DIR.
