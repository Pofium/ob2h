# Tasks: pin-dialog-mcp

- [ ] T-PIN.1 Подкоманда `secrets ask-pin` (hide): цель-парсинг `--purpose`,
      JSON-протокол stdout, коды выхода; юнит-тесты парсинга purpose/JSON
      (без GUI).
- [ ] T-PIN.2 Модуль `src/secrets/pin_spawn.rs`: спавн хелпера через
      `current_exe`, таймаут `OB2H_PIN_TIMEOUT_SECS`, enum `PinPromptOutcome`;
      гейт-функции; офлайн-тесты (env-гейты, парсер вывода, контракт ошибок).
- [ ] T-PIN.3 MCP `secrets_set`/`secrets_get`: при locked → spawn-диалог →
      unlock_with_key (derive по kv salt/kdf) → retry операции; Cancelled/
      timeout/unavailable → контрактовые ошибки; тесты диспетчера на стабах.
- [ ] T-PIN.4 Тесты: вывод хелпера (pin/cancelled/unavailable), TTL без окна,
      `OB2H_PIN_GUI=0` — прежнее поведение; GUI-окно — ручной приёмочный тест.
- [ ] T-PIN.5 Доки: docs/HERMES_INTEGRATION (UX-поток «секрет из чата»),
      CHANGELOG; журнал PLAN §6.
