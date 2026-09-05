---
name: claude-code
description: Claude Code CLI (Anthropic) on this machine, version 2.1.233. Use when developing/testing OB2H integration, skills, or MCP for Claude Code.
---

# Claude Code (Windows)

CLI-агент Anthropic. Версия: **2.1.233**. Один из 8 агентов из README OB2H.

## Установка / окружение
- Бинарник/обёртка: `~/bin/claude` (shell wrapper — pxpipe :47821 → Headroom
  :8787 → sqz :9999; приоритет над npm-бинарником).
- npm: `~/AppData/Roaming/npm/claude(.cmd|.ps1)`
- Домашняя папка: `~/.claude/` (settings.json, config.json, skills/, history.jsonl)

## Интеграция OB2H
- Скилл: `~/.claude/skills/ob2h/SKILL.md` (развёрнут `install_claude`)
- MCP: `~/.claude/config.json` → `mcpServers.ob2h` = `ob2h.exe serve`
- Команда установки из OB2H: `ob2h agent install --agent claude`
- Статус: `ob2h agent status` (проверяет `~/.claude/skills/ob2h/SKILL.md`)

## Ключевые команды скилла
- `/ob2h scan` — AST-скан кодовой базы
- `/ob2h report` — дайджест архитектуры (God Nodes)
- `/ob2h save <факт>` — сохранить факт/решение
- `/ob2h search <запрос>` — гибридный поиск

## Питфоллы
- Обёртка `~/bin/claude` использует compression proxy (pxpipe+Headroom+sqz).
- Не оборачивать сам ob2h в mcp-compressor (запрет пользователя).
