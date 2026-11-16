# Tasks: issue2-ux-live-fixes

- [ ] T-UX.1 `is_terminal()`-гейт в `resolve_pin`/`resolve_new_pin`
      (`src/cli/secrets.rs`): не-терминальный stdin → контрактовая ошибка без
      rpassword; юнит-тест логики гейта.
- [ ] T-UX.2 Контрактная ошибка `memory_feedback` без key (server.rs) + тест
      MCP-диспетчера.
- [ ] T-UX.3 `warming_seconds_left` в degraded-ответе memory_search: трекер
      прогрева в сервере, среднее прошлых прогревов в kv; тест.
- [ ] T-UX.4 Подкоманды `backup create|verify` при сохранении старых форм;
      snapshot-тест CLI-контракта обновлён; CHANGELOG.
