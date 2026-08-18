# Подключение OmnesMemory к Hermes

Конфиг Hermes: `C:\Users\ipres\AppData\Local\hermes\config.yaml`, блок `mcp_servers:`.
**Правку конфига делать вручную** (правило AGENTS.md §1: агенты конфиг Hermes не меняют).

## 1. Подготовка (однократно)

```bash
cd C:\Projects\omnesbot_for_hermes
python -m venv .venv
.venv\Scripts\pip install -e ".[local,docs]"
```

Проверить ручной запуск (сервер должен молча ждать stdin — это норма для stdio-MCP):

```bash
.venv\Scripts\python -m omnes_memory.server
```

## 2. Сниппет для `config.yaml`

Базовый вариант (появится в фазе 6 плана):

```yaml
mcp_servers:
  omnes-memory:
    command: C:/Projects/omnesbot_for_hermes/.venv/Scripts/python.exe
    args:
      - -m
      - omnes_memory.server
    env:
      OMNES_DATA_DIR: C:/Projects/omnesbot_for_hermes/data
      OMNES_LLM_BASE_URL: https://api.deepseek.com/v1
      OMNES_LLM_API_KEY: DEEPSEEK_API_KEY      # имя env-переменной с ключом
      OMNES_LLM_MODEL: deepseek-v4-flash
      OMNES_EMBED_PROVIDER: local              # или api + OMNES_EMBED_BASE_URL/KEY
```

Примечания:

- Hermes принимает имя переменной окружения вместо значения ключа
  (так работает `api_key: AITUNNEL_API_KEY` в существующем конфиге).
- Пробелов в пути нет — проблема spawn на Windows не возникает.
  Путь к python указывать полный, прямые слеши.

Вариант с компрессией вывода (как у остальных MCP-серверов пользователя,
через `mcp-compressor.exe`):

```yaml
mcp_servers:
  omnes-memory:
    command: C:\Users\ipres\.cargo\bin\mcp-compressor.exe
    args:
      - -c
      - medium
      - --
      - C:/Projects/omnesbot_for_hermes/.venv/Scripts/python.exe
      - -m
      - omnes_memory.server
    env:
      OMNES_DATA_DIR: C:/Projects/omnesbot_for_hermes/data
      # ... остальные OMNES_* те же
```

Компрессор полезен для `graph_reason`/`memory_context` с длинным выводом;
для отладки первый запуск лучше делать без него.

## 3. Проверка после подключения

1. Перезапустить Hermes.
2. Убедиться, что инструменты появились (в Hermes — список MCP-инструментов сервера
   `omnes-memory`).
3. Живой сценарий (из PLAN.md §6.4):
   - попросить Hermes «сохрани в память: …» → `memory_save`;
   - в **новом** чате спросить так, чтобы всплыл факт → `memory_search`;
   - подсунуть документ → `knowledge_extract` → вопрос по содержимому → `graph_reason`;
   - запустить `dream_run` → проверить `workspace/memory/MEMORY.md` и git-историю
     (`dream_log` / `dream_restore`).
4. Логи при проблемах: `C:\Projects\omnesbot_for_hermes\logs\omnes-memory.log`.

## 4. Ограничения жизненного цикла

- Сервер живёт, пока живёт Hermes (stdio). Фоновый автодрим работает в это же время.
  Если Hermes выключен надолго — дрим можно запустить вручную:
  `.venv\Scripts\python -m omnes_memory.dream_cli run` (появится в фазе 5, если
  понадобится).
- Все данные — в `OMNES_DATA_DIR`. Перенос на другую машину = скопировать папку
  проекта + `data/` + установить зависимости.
