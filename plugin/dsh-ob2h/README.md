# dsh-ob2h — cordis-плагин памяти для DeepSeek Harness

Тонкий мост между DSH и OB2H: даёт агенту то, чего у харнесса нет из коробки —
**автоматический recall** перед ходом и **автоматический захват ходов** после него.
Это DSH-аналог Hermes-MemoryProvider (`plugin/ob2h/`, Python), повторяющий его
поведение штатными средствами харнесса.

Инструменты (`mcp__ob2h__*`, 36 шт.) ставит отдельная строка патча —
`@deepseek-ai/dsh-mcp-client`; этот плагин инструментов не регистрирует и политику
инструментов не обходит.

## Что делает

| Механизм | Точка подключения | Поведение |
|---|---|---|
| **recall** | waterfall `agent/pre-step` (`{ prepend: true }`), `step === 1` | берёт последнее пользовательское сообщение хода → `memory_context` (fallback `memory_search`) → добавляет user-сообщение `source.kind: 'ob2h'` с блоком `<agent_memory>` |
| **capture** | `session/event` | собирает ход (`turn/start` → `user/message` с `source.kind === 'user'` → `assistant/message` → `turn/end`) и пишет пару через `session_ingest` полным префиксом сессии (сервер дедуплицирует по позиции) |
| **note** | `systemPrompt.section` (`order: 10300`) | статичная заметка: блок `<agent_memory>` — это память; явные операции — через MCP-инструменты ob2h |

Почему recall идёт в батч сообщений, а не в системный промпт: промпт собирается
**до** `agent/pre-step`, а его провайдеры секций синхронные — асинхронный запрос к
памяти там не выполнить. Так же поступает штатный `dsh-time-context`.

Почему заметка — статичный текст: `section.text` может быть функцией, но вызывается
без `await`, поэтому динамику туда не положить.

## Инварианты

- **Ноль внешних импортов** — только `node:crypto`, `node:child_process`,
  `node:readline`, относительные файлы. Профиль DSH может содержать junction'ы на
  чужой runtime (`~/.dsh/profiles/node_modules` → старый `0.1.2-rc.1`), а импорт
  по абсолютному пути не проходит через interception харнесса: любой
  `import '@deepseek-ai/...'` рискует упасть или подтянуть вторую копию пакета.
- **Ноль dsh-пиров** в `package.json` — гейт совместимости харнесса
  (`evaluatePluginCompatibility`) тогда не может «молча отключить» строку при
  обновлении DSH.
- **Fail-open**: любая ошибка памяти (нет бинарника, таймаут, битый JSON, падение
  child) логируется на уровне `debug`/`warn` и не влияет на ход.
- **Один долгоживущий child** `ob2h serve` на плагин, с backoff-перезапуском и
  остановкой при выгрузке фибера (`ctx.effect`).

## Установка

```bash
cd <репозиторий ob2h>
cargo build --release
./target/release/ob2h.exe agent install --agent dsh
```

Установщик сам добавит в `$DSH_HOME/profiles/<профиль>/cordis.patch.yml` строку
`insert` с абсолютным путём к `index.js` и нужным `config`. Проверить, что путь
корректен, можно так:

```bash
node -e "import('file:///C:/Projects/ob2h/plugin/dsh-ob2h/index.js').then(m => console.log(m.name, Object.keys(m)))"
```

## Конфиг (все поля необязательны)

| Поле | Дефолт | Смысл |
|---|---|---|
| `binary` | `''` | путь к `ob2h.exe`; пусто → плагин выключен (warning в лог) |
| `args` | `['serve']` | аргументы child-процесса |
| `cwd` | `''` | рабочий каталог child (от него ob2h ищет `.env` и `data/`) |
| `dataDir` | `''` | → `OB2H_DATA_DIR` в окружении child |
| `env` | `{}` | дополнительные переменные child |
| `source` / `author` | `dsh` / `dsh` | метки записей `session_ingest` |
| `note` | `true` | заметка в системном промпте |
| `capture` | `true` | автозахват ходов |
| `capturePrimaryOnly` | `true` | пропускать сессии сабагентов (fail-open) |
| `recall` | `true` | prefetch памяти перед ходом |
| `recallTimeoutMs` | `2500` | бюджет prefetch |
| `maxTokens` | `30` | сколько записей просить (`memory_context:max_tokens`) |
| `maxChars` | `4000` | бюджет символов блока (`memory_context:max_chars`) |
| `callTimeoutMs` | `900000` | таймаут одного RPC-вызова |
| `startupTimeoutMs` | `30000` | таймаут первого handshake |

## Структура

```
index.js          — точка входа cordis + чистые хелперы (apply/install/DEFAULTS)
lib/rpc.js        — stdio-клиент MCP JSON-RPC (start/call/toolCall/stop, backoff)
lib/provider.js   — Ob2hMemory: recordTurn/flush/recall/close + статистика
tests/fake_server.js — подставной ob2h serve (журнал вызовов, таймауты, пустые ответы)
tests/*.test.js   — rpc / provider / plugin (подставной cordis-ctx)
```

## Тесты

```bash
node --test "tests/*.test.js"                      # 22 теста, без DSH и без сети
$env:OB2H_LIVE='1'; node --test "tests/*.test.js"  # + 2 проверки против живого ob2h.exe
$env:OB2H_LIVE='1'; $env:OB2H_LIVE_WRITE='1'; node --test "tests/*.test.js"  # + запись в память
```

`install(ctx, options, resources)` вынесен из `apply` отдельно: тесты передают
подставной контекст cordis (см. `tests/plugin.test.js`) и подставной транспорт,
поэтому проверяются и инъекция блока, и запись хода, и фильтр сабагентов —
без запуска харнесса.
