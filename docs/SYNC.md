# Синхронизация двух инстансов ob2h (PC ↔ VPS)

> Память и граф знаний одного агента становятся доступны второму — без облаков,
> без сетевых сервисов и без открытых портов. Только ваши машины и SSH.

Версия: v0.9+ (ADR-9 в `PLAN.md` §6). Быстрая выжимка для README — в корне репо;
эта страница — полный гайд.

---

## 1. Кому это нужно

| Сценарий | Что даёт синхронизация |
|---|---|
| **Hermes на ПК + Hermes на VPS** (телеграм-бот / gateway на сервере) | Оба агента помнят одно и то же: рассказали дома о проекте — VPS-агент в Телеграме уже в контексте |
| **Рабочий и домашний компьютеры** | Общая память без общего облака: факты, предпочтения, граф знакомств/проектов |
| **Агент-ассистент на сервере + офлайн-работа с ноутбука** | VPS копит знания круглосуточно; ноутбук подтягивает их и отдаёт свои |
| **Бэкап-стратегия** | Каждая машина — живая реплика памяти другой (в дополнение к `ob2h backup`) |

**Что именно синхронизируется:** факты памяти (`memories`), узлы и рёбра графа знаний
(`graph_nodes`/`graph_edges`), удаления (tombstones).

**Что НЕ синхронизируется (сознательно):**
- сырые транскрипты диалогов (`daily/*.jsonl`) и консолидированная история
  (`history.jsonl`) — это личные дневники каждой машины;
- файлы личности (`SOUL.md`/`USER.md`/`MEMORY.md`) — у каждого агента свой характер
  и своя «выжимка», дрим перегенерирует их из общей БД;
- документы и чанки (`documents`/`chunks`) — большие и ре-ингестируемы.

## 2. Как это работает

```
        PC (Windows, за NAT)                        VPS (Linux, белый IP)
  ┌───────────────────────────┐            ┌────────────────────────────┐
  │ ob2h (origin=pc)          │            │ ob2h (origin=vps)          │
  │                           │  1. push   │                            │
  │ data/sync/outbox/*.gz ────┼── scp ─────┼─→ data/sync/inbox/         │
  │                           │            │        │ 2. apply-inbox    │
  │                           │            │        ▼    (таймер 04:30) │
  │                           │            │   import → БД              │
  │ data/sync/inbox/ ←────────┼── scp ─────┼── data/sync/outbox/*.gz    │
  │        │ 3. import        │  pull      │        ▲ 4. export         │
  │        ▼                  │            │        └── (таймер 04:30)  │
  └───────────────────────────┘            └────────────────────────────┘
```

- Единица обмена — **бандл**: gzip'd JSONL, инкрементальный (только изменённые
  строки с прошлого экспорта данному пиру). Watermark ведётся на пира.
- **Инициатива всегда на PC** — домашняя машина за NAT, сервер к ней не подключится.
  VPS по таймеру только применяет входящие (`apply-inbox`) и готовит исходящие
  (`export --peer pc`).
- **Никакая живая SQLite-БД не передаётся.** Синхронизировать `ob2h.db` файловыми
  синками (Syncthing/Dropbox/диск) категорически нельзя — WAL-файлы двух машин
  гарантированно коррумпируются. Переносится только папка бандлов `data/sync/`.

### Разрешение конфликтов (LWW)

Одну и ту же строку поправили на обеих машинах:

1. Побеждает большая `updated_at` (later write wins).
2. При равенстве — `origin` выше по списку `priority` из `peers.json`
   (например, `["pc", "vps"]` — при равенстве времени правка с ПК важнее).
3. Удаления — tombstones (`deleted_at`): «забытый» факт скрывается везде;
   повторное сохранение того же ключа воскрешает факт.
4. Повторный импорт того же бандла — no-op (идемпотентность по `bundle_id`).
5. Перед применением каждого нового бандла создаётся авто-бэкап (`ob2h backup`).

### Факты «с другой машины»

Каждая строка несёт `origin` (pc/vps). Агент на Linux видит факты, заработанные на
Windows (пути `C:\...`, софт и т.п.) — это фича, а не баг: VPS-агент **знает о**
вашем десктопе, но не должен пытаться открывать чужие пути (скилл агента
предупреждает об этом). Поиск возвращает происхождение факта.

## 3. Настройка: пошагово

Предусловия: на обеих машинах установлен Hermes + ob2h v0.9+ и включён плагин
(`ob2h plugin install` + `memory.provider: ob2h` — см. `HERMES_INTEGRATION.md`).

### Шаг 1. SSH-доступ с PC на VPS

Проверьте, что `ssh <алиас>` и `scp` работают с ПК без пароля (ключ в
`~/.ssh/config`):

```bash
ssh my-vps "echo ok"     # должно напечатать ok
```

Алиас удобно описать в `~/.ssh/config`:

```
Host my-vps
    HostName 193.109.79.30
    User root
    Port 22
    IdentityFile ~/.ssh/id_ed25519
```

### Шаг 2. Конфиг пирингов на PC — `data/sync/peers.json`

(образец — `scripts/pc/peers.example.json`)

```json
{
  "origin": "pc",
  "priority": ["pc", "vps"],
  "after_dream": true,
  "peers": {
    "vps": {
      "method": "ssh",
      "host": "my-vps",
      "push_to": "/root/ob2h_data/sync/inbox",
      "pull_from": "/root/ob2h_data/sync/outbox"
    }
  }
}
```

| Поле | Смысл |
|---|---|
| `origin` | Имя этой машины (произвольное: `pc`, `laptop`, `vps`…) |
| `priority` | Порядок tie-break LWW; также нормализует пустой origin строк |
| `after_dream` | `true` — push+pull всех ssh-пиров автоматически после каждого автодрима |
| `peers.<имя>.method` | `ssh` (автоматически scp) или `manual` (перенос бандлов руками/Syncthing/git) |
| `host` | SSH-алиас или `user@host` из `~/.ssh/config` |
| `ssh_port` | Порт, если алиас не описан в ssh-config |
| `push_to` / `pull_from` | Папки на пире: куда класть свои / откуда забирать чужие бандлы |

### Шаг 3. Конфиг на VPS — `/root/ob2h_data/sync/peers.json`

```json
{ "origin": "vps", "priority": ["pc", "vps"], "after_dream": false, "peers": {} }
```

VPS не ходит на PC сам — запись о пире не нужна, `origin` обязателен
(участвует в LWW и в именах бандлов).

```bash
mkdir -p /root/ob2h_data/sync/inbox /root/ob2h_data/sync/outbox
```

### Шаг 4. Расписание

**VPS** — systemd-таймер (юниты в `scripts/vps/`): ежедневно применяет inbox
и готовит outbox.

```bash
cd <репо ob2h>/scripts/vps
cp ob2h-sync.service ob2h-sync.timer /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now ob2h-sync.timer
```

**PC** — два механизма (можно оба):
- `after_dream: true` в peers.json — обмен после каждого автодрима (пока Hermes запущен);
- задача планировщика (ежедневно, работает и при выключенном Hermes):

```powershell
powershell -File scripts/pc/sync-task.ps1 -Register   # регистрация
powershell -File scripts/pc/sync-task.ps1             # разовый запуск
```

### Шаг 5. Первый обмен

```bash
# PC:
ob2h sync push --peer vps          # экспорт + scp на VPS
# VPS:
ob2h sync apply-inbox && ob2h sync export --peer pc
# PC:
ob2h sync pull --peer vps          # забрать бандлы VPS + применить
ob2h sync status                   # состояние обеих сторон
```

Дальше всё происходит само по расписанию.

## 4. Режим `manual` (без SSH)

Если машины не соединяются напрямую (обе за NAT, параноидальный firewall):

1. В peers.json укажите `"method": "manual"`.
2. Бандлы кладутся в `data/sync/outbox/` командой `ob2h sync export --peer <имя>`.
3. Переносите папку бандлов любым способом: Syncthing, приватный git-репозиторий,
   флешка. **Только бандлы — не БД!**
4. На второй машине: положить в `data/sync/inbox/` → `ob2h sync apply-inbox`.

Приватность: бандлы содержат личные данные. Для git-репозитория — только приватный;
при желании шифруйте (age/gpg) — на 0.9 встроенного шифрования нет (бэклог).

## 5. Команды `ob2h sync`

| Команда | Что делает |
|---|---|
| `status` | origin, приоритеты, watermark'ы, счётчики бандлов |
| `export --peer <имя>` | выгрузить изменения в `data/sync/outbox/` (watermark на пира) |
| `import <файл...>` | применить конкретные бандлы |
| `apply-inbox` | применить всё из `data/sync/inbox/` |
| `push --peer <имя>` | export + scp на пир (method=ssh) |
| `pull --peer <имя>` | scp от пира + apply-inbox (method=ssh) |

## 6. Безопасность и приватность

- Трафик — внутри вашего SSH (шифрование транспорта уже есть). Ключи — обычный
  `~/.ssh`; ob2h не хранит паролей и не открывает портов.
- Персональные данные покидают машину **только** в бандлах и только на
  настроенные вами хосты. Никакой телеметрии.
- Авто-бэкап перед каждым новым бандлом + ротация 14 копий — откат возможен на
  любое состояние до импорта.

## 7. Troubleshooting

| Симптом | Причина / решение |
|---|---|
| `scp: dest open ... Failure` | На пире нет папки `push_to` — `mkdir -p` её |
| `пир 'vps' не найден в peers.json` | Опечатка в имени или файл не создан в `data/sync/` машины |
| Push/pull молча не делают ничего | `after_dream: false` и нет задачи планировщика — запустите руками или настройте расписание |
| `conflicts_проиграно=N` в выводе импорта | Норма: N локальных правок оказались старее входящих (LWW) |
| Один и тот же факт «прыгает» между машинами | Часовые марки `updated_at` сравниваются как UTC-строки — проверьте, что время на машинах синхронизировано (NTP) |
| Бандл не применяется повторно | Так задумано: `bundle_id` уже в `applied_bundles` — идемпотентность |
| `sync отключён до исправления` в логах | Битый `peers.json` (JSON-ошибка) — память работает, синк ждёт исправления |

## 8. Схема БД для синка (M2)

Миграция применяется автоматически при первом запуске v0.9+ (бэкап
`data/backups/pre-v08-*.db` создаётся перед ней):

```sql
ALTER TABLE memories    ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE memories    ADD COLUMN deleted_at TEXT;            -- tombstone
ALTER TABLE graph_nodes ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE graph_nodes ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_edges ADD COLUMN origin TEXT NOT NULL DEFAULT '';
ALTER TABLE graph_edges ADD COLUMN deleted_at TEXT;
ALTER TABLE graph_edges ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';

CREATE TABLE sync_state (
  peer TEXT PRIMARY KEY,             -- имя пира (watermark) или '__imports'
  last_export_at TEXT,
  last_import_at TEXT,
  applied_bundles TEXT NOT NULL DEFAULT '[]'
);
```

`origin=''` означает «строка создана/изменена этим узлом» — при экспорте
нормализуется в `origin` из peers.json. Локальная правка импортированной строки
сбрасывает `origin` обратно в `''`.
