# Proposal: PIN-диалог сейфа при secrets_set/get из MCP (issue #1)

Источник: [issue #1](https://github.com/Pofium/ob2h/issues/1) (enhancement).
Сценарий владельца: «дал логин/пароль в чате → агент вызывает `secrets_set` →
вылетает PIN-окно сейфа → пользователь вводит PIN → секрет сохранён» (и
зеркально при `secrets_get` в пределах TTL).

Сейчас MCP при locked возвращает ошибку «запустите ob2h secrets unlock»:
инициатором разблокировки всегда был человек в CLI (ADR-18). PIN-окно есть
(`src/secrets/pin_gui.rs`), но живёт только в CLI-пути и требует главного
потока процесса — MCP-сервер stdio-процесс, окно из него поднять нельзя.

## Решение: spawn-хелпер через тот же бинарник

1. **Скрытая подкоманда** `ob2h secrets ask-pin [--purpose unlock|new|confirm]`
   (`src/cli/secrets.rs`, `hide = true`): поднимает окно eframe как главный
   поток нового процесса; результат — JSON в stdout
   (`{"outcome":"pin","pin":"1234"}` / `{"outcome":"cancelled"}`); сборка без
   feature `gui` / headless / `OB2H_PIN_GUI=0` — JSON `{"outcome":"unavailable"}`
   + ненулевой exit-код. PIN в логи не пишется (ADR-21).
2. **MCP `secrets_set`/`secrets_get` при locked** (`src/mcp/server.rs`): если
   гейт разрешает (`OB2H_PIN_GUI!=0`, проверка `gui_disabled_by_env()`), сервер
   спавнит `current_exe() secrets ask-pin` (tokio::process, таймаут
   `OB2H_PIN_TIMEOUT_SECS`, дефолт 60):
   - PIN получен → derive_key по kv salt/kdf → `unlock_with_key` (TTL сохраняется:
     повторные вызовы в пределах TTL — без окна) → операция выполняется;
   - `cancelled` → `[Error] ввод PIN отменён пользователем` (без ретраев окна);
   - `unavailable`/timeout → прежняя ошибка с подсказкой про CLI unlock.
3. Распространяется только на `secrets_set`/`secrets_get` (`secrets_delete`,
   `secrets_status` не трогаются). Побочный процесс обменивается с сервером
   только через локальный pipe; IPC-рассылка другим процессам не нужна — CLI
   процессы выводят ключ из PIN самостоятельно (см. journal).

## Гейты и безопасность

- Гейт: `OB2H_PIN_GUI=1` (default) / `0` — фолбэк на поведение v2.0.0.
- Таймаут окна: `OB2H_PIN_TIMEOUT_SECS` (default 60).
- PIN никогда не в чате/логах/аудите (в audit — только имена, ADR-21).
- Окно локальное, без сети (ADR-18 не нарушается: ввод локален в процессе ob2h).

## Не-цели

- `secrets init`/`rekey` из MCP (инициализация — по-прежнему CLI).
- Биометрия/длинные PIN.

## Критерии приёмки

- [ ] `secrets_set` при locked открывает окно → PIN → секрет сохранён;
      `secrets_get` в TTL — без окна.
- [ ] `OB2H_PIN_GUI=0` → поведение идентично v2.0.0 (ошибка с подсказкой).
- [ ] Cancelled → `[Error] ввод PIN отменён пользователем`, без ретраев.
- [ ] После разблокировки окном параллельный CLI с тем же PIN работает.
- [ ] Тесты: парсинг вывода хелпера, гейты, Cancelled/timeout, headless-контракт.
- [ ] Доки: docs/HERMES_INTEGRATION — новый UX-поток; CHANGELOG.
