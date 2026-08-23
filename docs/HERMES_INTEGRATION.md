# Подключение OB2H к Hermes

Конфиг Hermes: Windows `%LOCALAPPDATA%\hermes\config.yaml`, Linux/VPS `~/.hermes/config.yaml`.
**Правку конфига Hermes делать вручную** (правило AGENTS.md §1); `ob2h install` — исключение,
явно запускаемое владельцем.

## Обзор режимов

| Режим | Что нужно | Захват диалогов | Recall |
|---|---|---|---|
| **A: плагин** (рекомендуемый) | `ob2h plugin install` + `memory.provider: ob2h` | автоматический (каждый ход) | автоматический (`<agent_memory>` + 🧠) |
| **B: плагин + MCP** | оба блока в конфиге | автоматический | автоматический + инструменты `mcp__ob2h__*` |
| **0: MCP-only** | `ob2h install` (mcp_servers) | только по инициативе модели | только по инициативе модели |

## Режим A/B: MemoryProvider-плагин (v0.8+)

```bash
cd <репозиторий ob2h>          # важно: data_dir пинится от cwd
ob2h plugin install
```

Что делает: копирует плагин в `$HERMES_HOME/plugins/ob2h/`, создаёт `ob2h.json`
с путями бинарника и data-Dir. Конфиг Hermes **не правит** — вставьте в существующий
блок `memory:` config.yaml:

```yaml
memory:
  provider: ob2h
```

Перезапустите Hermes. Проверка: `ob2h plugin status` и `hermes memory status`
(Provider: ob2h — installed ✓ available ✓). Плагин автоматически:
пишет каждый ход (`session_ingest`), инжектит `<agent_memory>` перед ходом,
отдаёт инструменты графа/дрима, переживает падение ob2h (рестарт с backoff).

В Mode A удалите блок `mcp_servers.ob2h` из config.yaml.

## Режим 0: MCP-only (как в v0.7.1)

```bash
ob2h install   # регистрирует mcp_servers.ob2h с бэкапом config.yaml
```

## Скилл агента

```bash
ob2h skill install   # деплой skills/ob2h/SKILL.md в $HERMES_HOME/skills/devops/ob2h/
                     # с подстановкой путей этой машины
```

## Синхронизация двух машин (PC ↔ VPS, v0.9+)

Обмен — инкрементальные gzip-бандлы (memories + граф) поверх SSH; LWW по `updated_at`,
удаления — tombstones; идемпотентно; авто-бэкап перед каждым новым бандлом.
**Инициатива всегда на PC** (VPS за ним не может ходить — PC за NAT).

1. **PC**: `data/sync/peers.json` (образец `scripts/pc/peers.example.json`):

```json
{
  "origin": "pc",
  "priority": ["pc", "vps"],
  "after_dream": true,
  "peers": {
    "vps": { "method": "ssh", "host": "vps-alt",
             "push_to": "/root/ob2h_data/sync/inbox",
             "pull_from": "/root/ob2h_data/sync/outbox" }
  }
}
```

`host` — SSH-алиас из `~/.ssh/config` (порт/ключ резолвит сам ssh).

2. **VPS**: `/root/ob2h_data/sync/peers.json`:

```json
{ "origin": "vps", "priority": ["pc", "vps"], "after_dream": false, "peers": {} }
```

и таймер `scripts/vps/` (apply-inbox + export ежедневно в 04:30):

```bash
cp ob2h-sync.service ob2h-sync.timer /etc/systemd/system/ && systemctl enable --now ob2h-sync.timer
```

3. **PC**: обмен — `ob2h sync push --peer vps && ob2h sync pull --peer vps`
   (или `scripts/pc/sync-task.ps1`, регистрация в планировщике: `-Register`;
   `after_dream: true` делает это автоматически после каждого автодрима).

Диагностика: `ob2h sync status` на обеих машинах.
**Никогда не синхронизируйте живую `ob2h.db` файловыми синками (Syncthing/Dropbox) —
только папку бандлов `data/sync/`.**

## Обновление бинарника

- **Windows**: остановить ob2h (Hermes держит exe) → `cargo build --release` →
  `ob2h plugin install && ob2h skill install` из корня репо → перезапуск Hermes.
- **Linux/VPS**: `git pull && cargo build --release && install -m755 target/release/ob2h
  /usr/local/bin/ob2h` (замена живого файла безопасна) → `ob2h plugin install &&
  ob2h skill install` → `systemctl --user restart hermes-gateway`.
- Миграция БД (например M2 в v0.9) применяется автоматически при первом запуске,
  перед ней создаётся бэкап `data/backups/pre-v08-*.db`. Даунгрейт безопасен
  (новые колонки имеют DEFAULT).

## Проверка после подключения

1. Диалог без упоминания памяти → в `data/workspace/daily/` растёт лог, в ответах 🧠.
2. `memory_save` → в новом чате вопрос «что ты знаешь про …» → факт всплывает.
3. Документ → `knowledge_extract` → `graph_reason`.
4. `ob2h stats`, `ob2h dream status`, `ob2h sync status`.
5. Логи при проблемах: `data/logs/ob2h.log`.
