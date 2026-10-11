# Подключение OB2H к DSH (DeepSeek Harness)

DSH — харнесс, внутри которого живёт агент (`dsh web` / Electron-GUI, `dsh tui`,
`dsh headless`). В отличие от Hermes, у DSH нет MemoryProvider-контракта, поэтому
память подключается двумя независимыми слоями:

| Слой | Что даёт | Где живёт |
|---|---|---|
| **MCP-сервер** (`@deepseek-ai/dsh-mcp-client`) | 36 инструментов `mcp__ob2h__*` — явные операции | запись `insert` в `cordis.patch.yml` профиля |
| **cordis-плагин** `plugin/dsh-ob2h` | авто-захват ходов (`session_ingest`), авто-prefetch памяти блоком `<agent_memory>`, заметка в системном промпте | вторая запись `insert` в том же блоке |
| **Скилл** `ob2h` | знание, когда памятью пользоваться, и что блок `<agent_memory>` уже пришёл | `$DSH_HOME/skills/ob2h/SKILL.md` |

Автозахват и prefetch — это и есть аналог Hermes-MemoryProvider, но сделанный
штатными средствами DSH: плагин слушает `session/event` и waterfall
`agent/pre-step`. Всё fail-open: память не может сломать ход диалога.

> ADR: **ADR-16** (`PLAN.md` §6) — интеграция с DSH живёт в patch-слое профиля,
> бинарник и данные общие с Hermes-инсталляцией, никаких новых сетевых портов.
> **ADR-17** — cordis-плагин `plugin/dsh-ob2h` (JS, ноль внешних зависимостей)
> как носитель автозахвата и prefetch; MCP-строка остаётся поверхностью инструментов.

## Быстрый старт

```bash
cd <репозиторий ob2h>            # data_dir пинится от cwd (или задайте OB2H_DATA_DIR)
cargo build --release
./target/release/ob2h.exe agent install --agent dsh
./target/release/ob2h.exe agent status     # строка «DSH (DeepSeek Harness)»
```

Команда идемпотентна и делает три вещи:

1. **MCP-сервер** — вставляет (или обновляет) блок между маркерами
   `# >>> ob2h:begin` … `# <<< ob2h:end` в
   `$DSH_HOME/profiles/<профиль>/cordis.patch.yml`: строку `mcp-ob2h`
   (инструменты) и строку `ob2h-memory` со **абсолютным путём** к
   `plugin/dsh-ob2h/index.js` (автозахват + prefetch).
   Остальные патчи профиля не трогаются; ручная запись с `serverName: ob2h`
   не дублируется.
2. **Скилл** — кладёт `SKILL.md` (исходник `skills/dsh/SKILL.md`, пути
   темплейтятся под эту машину) в `$DSH_HOME/skills/ob2h/SKILL.md` — это
   пользовательский корень скиллов DSH, доступный из любого рабочего каталога.
3. **Проверка ключа LLM** — если `OB2H_LLM_API_KEY` не виден ни в окружении, ни
   в `<репозиторий>/.env`, установщик предупредит (LLM-инструменты в этом случае
   отключены, детерминированные работают).

Если исходник плагина не найден рядом с `Cargo.toml`, установщик поставит только
MCP-строку и скажет об этом — интеграция деградирует до режима 0, а не падает.

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
# >>> ob2h:begin — блок управляется `ob2h agent install --agent dsh`; ручные правки теряются
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
    - id: ob2h-memory
      name: 'C:/Projects/ob2h/plugin/dsh-ob2h/index.js'   # абсолютный путь: 0.2.0-rc.2 его резолвит
      config:
        binary: 'C:/Projects/ob2h/target/release/ob2h.exe'
        cwd: 'C:/Projects/ob2h'
        dataDir: 'C:/Projects/ob2h/data'
        source: 'dsh'
        author: 'dsh'
# <<< ob2h:end — конец блока ob2h
```

## Про ключ LLM

В файле патча секрета нет вообще. Ключ ob2h читает сам — из окружения процесса
или из `.env` в **рабочем каталоге** (`config.rs::from_env` вызывает
`dotenvy::dotenv()`; cwd ребёнка задан патчем). Поэтому штатный путь — файл
`<репозиторий>/.env` (он в `.gitignore`):

```ini
# C:\Projects\ob2h\.env
OB2H_LLM_API_KEY=sk-...
OB2H_LLM_BASE_URL=https://api.deepseek.com/v1
OB2H_LLM_MODEL=deepseek-v4-flash
```

Почему не `config.env` в патче: DSH «вычищает» из окружения ребёнка переменные с
`KEY|TOKEN|SECRET` и все `DSH_*`, поэтому пробрасывать ключ приходится явно, а
`!!js`-выражение с пустым фолбэком (`process.env.OB2H_LLM_API_KEY ?? ''`)
**вредно**: пустая, но установленная переменная мешает `dotenvy` подхватить `.env`
(`dotenvy` не перезаписывает уже существующие переменные). Если ключ нужен именно
в окружении хоста — задайте его до старта DSH, и `.env` не понадобится:

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

## Режим 1: cordis-плагин `dsh-ob2h`

Исходник — `plugin/dsh-ob2h/` (ESM, **только** `node:*` и относительные импорты:
в профиле DSH лежат junction'ы на старый runtime 0.1.2-rc.1, и любой импорт
`@deepseek-ai/*` рискует подтянуть вторую копию пакета).

Что делает плагин:

1. **prefetch** — на первом шаге хода берёт последнее пользовательское сообщение,
   спрашивает у ob2h `memory_context` (таймаут по умолчанию 2,5 с) и подмешивает
   результат отдельным user-сообщением с `source.kind: 'ob2h'`
   (`<agent_memory>…</agent_memory>`). Если контекста нет — падает обратно на
   `memory_search`. Ровно один раз на ход, после `await next()` в waterfall
   `agent/pre-step` (`{ prepend: true }`) — так же, как штатный `dsh-time-context`.
   Почему не секция системного промпта: промпт собирается **до** pre-step, а его
   провайдеры синхронные — асинхронный recall туда не влезает и дал бы ход задержки.
2. **capture** — по событиям сессии собирает ход (`turn/start` → `user/message`
   с `source.kind === 'user'` → `assistant/message` → `turn/end`) и на закрытии
   хода пишет пару в ob2h через `session_ingest`, отдавая **полный префикс**
   сессии: сервер дедуплицирует по `(session_id, позиция)`. Ходы сабагентов
   (у которых есть владелец в `ctx.get('agents')`) пропускаются, инъекции
   плагинов в память не попадают.
3. **заметка** — статичная секция системного промпта (`name: ob2h:note`,
   `order: 10300`) о том, что блок `<agent_memory>` — это память, и какие
   MCP-инструменты доступны.

Конфиг плагина (все поля необязательны, дефолты — в `index.js::DEFAULTS`):

| Поле | Дефолт | Смысл |
|---|---|---|
| `binary` | — | путь к `ob2h.exe` (пусто → плагин выключен, пишет warning) |
| `args` / `cwd` / `dataDir` / `env` | `['serve']` / — / — / `{}` | как запускать дочерний `ob2h serve` |
| `source` / `author` | `dsh` / `dsh` | метки записей `session_ingest` |
| `note` / `capture` / `recall` | `true` | включить заметку / автозахват / prefetch |
| `capturePrimaryOnly` | `true` | не писать ходы сабагентов (fail-open: если `agents` нет — пишем) |
| `recallTimeoutMs` | `2500` | бюджет prefetch; не успел — ход идёт без памяти |
| `maxTokens` / `maxChars` | `30` / `4000` | сколько записей и сколько символов просить у `memory_context` |
| `callTimeoutMs` / `startupTimeoutMs` | `900000` / `30000` | таймауты RPC и первого handshake |
| `logFile` | `<dataDir>/logs/dsh-plugin.log` | куда писать собственный лог плагина (в Desktop вывод харнесса на диск не попадает) |
| `logEvents` | `false` | трассировать в лог каждое событие сессии (диагностика) |

Свой `ob2h serve` плагин держит **долгоживущим** (handshake ~1 с, дальше вызовы
десятки миллисекунд) и перезапускает с backoff при падении; при выгрузке фибера
child останавливается. То есть из DSH живут **два** дочерних ob2h — MCP-строка и
плагин; общая БД это выдерживает (WAL + `busy_timeout`), но одновременный
`dream_run` из двух процессов запускать нельзя (см. «Ограничения»).

### Живая проверка на настоящем потоке событий

Плагин подписан на `session/event`, поэтому его работу можно проверить не «на глаз»,
а реплеем реальных событий сессии: сессии DSH лежат в
`<DSH_HOME>/sessions/<проект>/<session-id>/session.v4.jsonl.zstd` (zstd, читается
`python -c "from compression import zstd; ..."` или любым zstd). Реплей через
`install()` с подставным `ob2h serve` показывает ровно то, что уйдёт в
`session_ingest`: выгрузить события хода (границы — `turn/start` … `turn/end` нужного
хода) в JSON и подать их в подставной `ctx` по образцу
`plugin/dsh-ob2h/tests/plugin.test.js`, затем прочитать журнал подставного сервера
(`test_journal`). Такие ad-hoc скрипты держатся в `data/tmp/` (gitignored) и
воспроизводятся по этому описанию за пару минут.

Замер на реальном длинном ходе (985 событий, 173 `assistant/message`): ровно один
вызов `session_ingest`, `user` = обе реплики человека хода, `assistant` ≈ 9,7 тыс.
символов, `stats.errors = 0`.

Тесты плагина (без DSH и без сети, подставной сервер — `tests/fake_server.js`):

```bash
cd plugin/dsh-ob2h && node --test "tests/*.test.js"
$env:OB2H_LIVE='1'; node --test "tests/*.test.js"   # + проверки против живого ob2h.exe
```

## Проверка

```bash
ob2h agent status                             # DSH — «Подключен» → ...cordis.patch.yml
dsh --profile desktop --dump-config | rg -A4 mcp-ob2h   # патч скомпоновался
cd plugin/dsh-ob2h && node --test "tests/*.test.js"      # 22 теста плагина
```

В живой сессии DSH: спросите «что ты знаешь про <тема>» — блок `<agent_memory>`
придёт сам (плюс агент может вызвать `mcp__ob2h__memory_search`); затем попросите
запомнить факт и проверьте его в новой сессии. Скилл `ob2h` появляется в каталоге
`<available_skills>`.

Признаки, что плагин действительно загрузился:

```bash
# дочерние процессы ob2h: их должно быть два (mcp-клиент и плагин) плюс процессы Hermes
Get-CimInstance Win32_Process -Filter "Name='ob2h.exe'" | Select ProcessId, ParentProcessId, CommandLine
# в логе ob2h видны записи session_ingest от источника dsh
Select-String -Path data\logs\ob2h.log -Pattern 'session_ingest' | Select -Last 5
```

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

- **Автозахват есть, но не «вечный».** Пишется только то, что прошло как
  завершённый ход DSH с настоящим пользовательским вводом; диалоги внутри
  инструментов и ходы сабагентов не пишутся.
- **Правки кода плагина — только после перезапуска DSH.** Cordis-HMR переприменяет
  строку патча при изменении **конфига** (дочерний `ob2h serve` пересоздаётся), но
  модуль, подключённый абсолютным путём, остаётся в кэше ESM: новый код заработает
  лишь при следующем старте харнесса. Правка, сделанная посреди хода, вдобавок
  обнуляет состояние захвата до следующего `turn/start`.
- **Ход, в середине которого плагин поднялся, не записывается.** `turn/start` этого
  хода плагин не видел → на `turn/end` нет состояния; в логе плагина это видно как
  `пропущен — нет состояния (ход начался до загрузки плагина)`. Теряется ровно один
  ход — тот, во время которого ставили/перезапускали плагин.
- **Prefetch — с бюджетом 2,5 с и без блокировки хода**: если ob2h занят
  (например, идёт дриминг), ход просто уйдёт без блока памяти.
- **Общая БД с Hermes.** `data/ob2h.db` — тот же файл; WAL + `busy_timeout`
  выдерживают несколько процессов, но `dream_run` из двух процессов одновременно
  не запускать. Если нужно запретить автодрим у одного из детей DSH —
  добавьте `OB2H_AUTODREAM_ENABLED: 'false'` в `env` соответствующей строки.
- **Инструменты MCP не видны сразу после правки патча**: плагин
  `dsh-mcp-client` подключается при загрузке дерева; `patchReload: live`
  перезапускает слои, но если сервер не появился — перезапуск DSH.
- **Строка плагина необязательна**: если DSH не сможет её активировать, старт
  не сломается (строка не входит в `requiredStartupEntryIds`), автозахват просто
  не заработает — смотрите `<DSH_HOME>/dsh.log`.
- **Resources/Prompts MCP не бриджатся** — `dsh-mcp-client` отдаёт только tools.
- **Скилл не темплейтится повторно сам**: после смены пути exe/данных повторите
  `ob2h agent install --agent dsh` (тогда же обновится и путь к плагину).
- **Обновление DSH.** Патч резолвит путь к плагину в 0.2.0-rc.2+; в старых
  сборках абсолютные пути в `insert` не поддерживались. Плагин не объявляет
  `peerDependencies` на `@deepseek-ai/dsh-*`, поэтому гейт совместимости его не
  отключает; при смене внутренних API плагина харнессом он деградирует до
  режима 0 (MCP продолжает работать).

## Если что-то не работает

| Симптом | Причина / лечение |
|---|---|
| Инструментов `mcp__ob2h__*` нет | Патч не скомпоновался: `dsh --profile desktop --dump-config`; посмотрите `<DSH_HOME>/dsh-server.log` на ошибки `dsh-mcp-client`; перезапустите DSH |
| Блок `<agent_memory>` не приходит | Плагин не загрузился или не нашёл `binary`: `<DSH_HOME>/dsh.log`, `data/logs/ob2h.log`; проверьте `Get-CimInstance Win32_Process` — должен быть второй дочерний `ob2h.exe`. Если пусто и в памяти — это норма: recall не выдумывает факты |
| Ходы не попадают в память | Сначала `data/logs/dsh-plugin.log` (собственный лог плагина, по строке на ход: `записан ход` / `пропущен — …`). Если файла нет — плагин не активировался (строка не загрузилась: `<DSH_HOME>/dsh.log`) либо в коде до файлового лога не дошло (нужен перезапуск DSH после обновления плагина). Если в логе `пропущен — нет состояния` — ход начался раньше загрузки плагина, следующий ход запишется. Затем `data/logs/ob2h.log` (`session_ingest`), и только потом проверяйте `capture: false` / сессию сабагента |
| «Unknown tool» при вызове `memory_save` | Вызов идёт без неймспейса: нужно `mcp__ob2h__memory_save` |
| `dream_run` / `knowledge_extract` падают с 401/403 | Нет `OB2H_LLM_API_KEY` ни в окружении, ни в `<репозиторий>/.env` (см. выше); лог — `data/logs/ob2h.log` |
| Сервер поднимается долго | Первый старт грузит локальные Candle-эмбеддинги (`~/.cache/huggingface`), дальше ~1–2 с |
| Пустая память/граф не тот проект | Проверьте `OB2H_DATA_DIR` и cwd: сервер пинит `data/` от cwd, а `project_*` требует явного `id` |
