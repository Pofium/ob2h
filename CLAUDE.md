# CLAUDE.md

Правила проекта — в [`AGENTS.md`](AGENTS.md). Прочитать его целиком перед началом работы.

Кратко:

- Источник задач — [`PLAN.md`](PLAN.md) (первая незакрытая задача).
- Архитектура и схема хранения — [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md);
  что и откуда портировать из omnes-aibot — [`docs/REFERENCE_omnesbot.md`](docs/REFERENCE_omnesbot.md).
- Границы: только SQLite, без torch/Postgres/Neo4j/Docker, один пользователь.
- `C:\Users\ipres\AppData\Local\hermes\config.yaml` не менять без явного указания пользователя.
- Перед коммитом: `pytest` зелёные, `ruff check` чисто, чекбокс задачи закрыт в PLAN.md.
