# Подключение OB2H к DSH (DeepSeek Harness)

DSH — харнесс, внутри которого живёт агент (`dsh web` / Electron-GUI, `dsh tui`,
`dsh headless`). В отличие от Hermes, у DSH нет MemoryProvider-контракта: память
подключается **MCP-сервером** через штатный плагин DSH `@deepseek-ai/dsh-mcp-client`
(в поставке харнесса, включается одной записью `insert` в patch-слое профиля),
а знания о том, когда памятью пользоваться, даёт **скилл** DSH.

Итог: инструменты видны модели как `mcp__ob2h__<имя>` (36 шт.), скилл `ob2h` —
в каталоге скиллов сессии. Автозахвата ходов нет (это режим 0, MCP-only):
писать в память нужно явными вызовами `session_log` / `memory_save`.

> ADR: **ADR-16** (`PLAN.md` §6) — интеграция с DSH живёт в patch-слое профиля,
> бинарник и данные общие с Hermes-инсталляцией, никаких новых сетевых портов.

## Быстрый старт

```bash
cd <репозиторий ob2h>            # data_dir пинится от cwd (или задайте OB2H_DATA_DIR)
cargo build --release
./target/release/ob2h.exe agent install --agent dsh
./target/release/ob2h.exe agent status     # строка «DSH (DeepSeek Harness)»
```

Команда идемпотентна и делает две вещи:

1. **MCP-сервер** — вставляет (или обновляет) блок `mcp-ob2h` между маркерами
   `# >>> ob2h:begin` … `# <<< ob2h:end` в
   `$DSH_HOME/profiles/<профиль>/cordis.patch.yml`. Остальные патчи профиля
   не трогаются; ручная запись с `serverName: ob2h` не дублируется.
2. **Скилл** — кладёт `SKILL.md` (исходник `skills/dsh/SKILL.md`, пути
   темплейтятся под эту машину) в `$DSH_HOME/skills/ob2h/SKILL.md` — это
   пользовательский корень скиллов DSH, доступный из любого рабочего каталога.

Профиль выбирается так: `$DSH_PROFILE` → `desktop` → `web` → первый по алфавиту;
`$DSH_HOME` → `~/.dsh`. Кастомный профиль можно указать явно:
`ob2h agent install --agent dsh --path C:/Users/<u>/.dsh/profiles/my`.

## Перезапуск

Профили с `patchReload: live` (кастомные, включая `desktop`) подхватывают
изменённый `cordis.patch.yml` без перезапуска — `dsh` watch'ит этот файл.
Для профилей со `startup`-режимом (`web`, `tui`, `headless` из шаблонов)
нужен перезапуск DSH. Первое подключение MCP-сервера всегда идёт на старте
хоста: если инструментов не видно — перезапустите DSH.

## Что попадает в патч

```yaml
- insert:
    - id: mcp-ob2h
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: ob2h
        transport: stdio
        command: 'C:/Projects/ob2h/target/release/ob2h.exe'
        args: ['serve']
        cwd: 'C:/Projects/ob2h'          # data_dir = <cwd>/data, логи и БД там же
        toolCallTimeoutMs: 300000        # project_scan/dream_run длиннее дефолтных 60 с
        env:
          OB2H_DATA_DIR: 'C:/Projects/ob2h/data'
          OB2H_LLM_BASE_URL: 'https://api.deepseek.com/v1'
          OB2H_LLM_MODEL: 'deepseek-v4-flash'
          OB2H_LLM_API_KEY: !!js process.env.OB2H_LLM_API_KEY ?? ''
```

### Про ключ LLM

В файле патча секрета нет: `!!js`-выражение вычисляется хостом DSH, а `config.env`
перекрывает «вычищенное» окружение ребёнка (DSH удаляет ambient-переменные с
`KEY|TOKEN|SECRET` и все `DSH_*`). Поэтому ключ нужно положить в окружение
**хоста** DSH, например:

```powershell
setx OB2H_LLM_API_KEY "sk-..."        # сам ключ
# или индирекция, как в Hermes:
setx OB2H_LLM_API_KEY DEEPSEEK_API_KEY
setx DEEPSEEK_API_KEY "sk-..."        # значение подхватит сам ob2h
```

Без ключа работают все детерминированные пути: `memory_save/search/context/update`,
`memory_feedback/merge`, `workspace_*`, `session_log/ingest`, `graph_search/stats`,
`project_*` (AST-скан без LLM), `ast_*`, `ralph_*`, `omnes_stats/backup`, `sync`.
LLM-инструменты (`dream_run`, `knowledge_extract`, `graph_reason`) вернут ошибку API.

## Проверка

```bash
ob2h agent status                             # DSH — «Подключен» → ...cordis.patch.yml
dsh --profile desktop --dump-config | rg -A3 mcp-ob2h   # патч скомпоновался
```

В живой сессии DSH: спросите «что ты знаешь про <тема>» (агент должен вызвать
`mcp__ob2h__memory_search`), затем попросите запомнить факт и проверьте его в
новой сессии. Скилл `ob2h` появляется в каталоге `<available_skills>`.

Полный smoke-тест сервера без DSH (handshake + список инструментов):

```bash
python - <<'PY'
import json, subprocess
p = subprocess.Popen([r"target/release/ob2h.exe", "serve"], cwd=".",
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
p.stdin.write(json.dumps({"jsonrpc":"2.0","id":1,"method":"initialize",
    "params":{"protocolVersion":"2024-11-05","capabilities":{},
              "clientInfo":{"name":"probe","version":"1"}}})+"\n"); p.stdin.flush()
print(p.stdout.readline())
PY
```

## Ограничения и отличия от Hermes-интеграции

- **Нет авто-захвата и авто-prefetch.** Режим 0: модель сама вызывает инструменты.
  Скилл `ob2h` учит, когда это делать; при желании авто-захват делается отдельным
  cordis-плагином (сессионные события + инъекция `<agent_memory>`) — пока не реализован.
- **Общая БД с Hermes.** `data/ob2h.db` — тот же файл; WAL + `busy_timeout`
  выдерживают два процесса, но `dream_run` из двух процессов одновременно не запускать.
- **Инструменты MCP не видны сразу после правки патча**: плагин `dsh-mcp-client`
  подключается при загрузке дерева; `patchReload: live` перезапускает слои, но если
  сервер не появился — перезапуск DSH.
- **Resources/Prompts MCP не бриджатся** — `dsh-mcp-client` отдаёт только tools.
- **Скилл не темплейтится повторно сам**: после смены пути exe/данных повторите
  `ob2h agent install --agent dsh`.

## Если что-то не работает

| Симптом | Причина / лечение |
|---|---|
| Инструментов `mcp__ob2h__*` нет | Патч не скомпоновался: `dsh --profile desktop --dump-config`; посмотрите `<DSH_HOME>/dsh-server.log` на ошибки `dsh-mcp-client`; перезапустите DSH |
| «Unknown tool» при вызове `memory_save` | Вызов идёт без неймспейса: нужно `mcp__ob2h__memory_save` |
| `dream_run` / `knowledge_extract` падают с 401/403 | Нет `OB2H_LLM_API_KEY` в окружении хоста DSH (см. выше); лог — `data/logs/ob2h.log` |
| Сервер поднимается долго | Первый старт грузит локальные Candle-эмбеддинги (`~/.cache/huggingface`), дальше ~1–2 с |
| Пустая память/граф не тот проект | Проверьте `OB2H_DATA_DIR` и cwd: сервер пинит `data/` от cwd, а `project_*` требует явного `id` |
