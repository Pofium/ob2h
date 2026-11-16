# Proposal: UX-фиксы живого теста (issue #2)

Источник: [issue #2](https://github.com/Pofium/ob2h/issues/2) (bug, живой тест
v2.0.0 на бою 16.11.2026). Четыре мелких UX-дефекта, исправляются одним
патч-циклом в составе релиза v2.0.1.

## Изменения

1. **CLI secrets без TTY — честная ошибка вместо зависания** (`src/cli/secrets.rs`).
   `resolve_pin`/`resolve_new_pin` в ветке `GuiPrompt::Unavailable` перед
   `rpassword::prompt_password` проверяют `stdin.is_terminal()`; если stdin — не
   терминал (автоматизация, agent-shell, `</dev/null`), вернуть контрактовую
   ошибку `[Error] PIN-запрос требует интерактивный терминал или GUI-сборку
   [Подсказка: …]` вместо вечного зависания.

2. **`memory_feedback` без key — подсказка** (`src/mcp/server.rs`). Ошибка
   `key is required` по контракту ошибок MCP:
   `[Error] key is required [Подсказка: key записи памяти из memory_search/memory_save; id уроков (lsn_*) не подходит]`.

3. **Degraded-режим с ETA** (`src/mcp/server.rs` + `src/embedding/mod.rs`).
   В ответ `memory_search` при `degraded_mode: "warming_up_embeddings"` добавить
   `warming_seconds_left` — оценка по среднему времени прогрева прошлых стартов
   (среднее хранится в kv `metrics.embedder_warm_avg_secs`, пишется при каждом
   завершённом прогреве; без истории — elapsed-секунды и пометка «первый прогрев»).

4. **`backup verify` — хелп и доки согласованы** (`src/cli/mod.rs`, `src/main.rs`).
   Подкомандная форма `ob2h backup create|verify` при сохранении старых форм
   (`ob2h backup --scope quick`, `ob2h backup --verify <path>`); хелп отражает
   оба синтаксиса. Изменение CLI-контракта (аддитивное) → запись в CHANGELOG.

## Не-цели

- Переписывание PIN-потока (issue #1 — отдельный цикл `pin-dialog-mcp`).
- Смена дефолтов TTL/гейтов.
- Замена rpassword на другой терминальный ввод.

## Критерии приёмки

- [ ] `ob2h secrets unlock </dev/null` завершается быстро с `[Error] …[Подсказка]`.
- [ ] `memory_feedback` без key возвращает контрактную подсказку.
- [ ] Degraded-ответ содержит оценку прогрева (или elapsed при первом прогреве).
- [ ] `ob2h backup create --scope quick`, `ob2h backup verify <path>` и старые
      формы работают; тесты зелёные.
