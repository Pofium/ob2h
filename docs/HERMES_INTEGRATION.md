# Подключение OB2H к Hermes

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
.venv\Scripts\python -m ob2h.server
```

## 2. Сниппет для `config.yaml`

Базовый вариант (появится в фазе 6 плана):

```yaml
mcp_servers:
  ob2h:
    command: C:/Projects/omnesbot_for_hermes/.venv/Scripts/python.exe
    args:
      - -m
      - ob2h.server
    env:
      OB2H_DATA_DIR: C:/Projects/omnesbot_for_hermes/data
      OB2H_LLM_BASE_URL: https://api.deepseek.com/v1
      OB2H_LLM_API_KEY: DEEPSEEK_API_KEY      # имя env-переменной с ключом
      OB2H_LLM_MODEL: deepseek-v4-flash
      OB2H_EMBED_PROVIDER: local              # или api + OB2H_EMBED_BASE_URL/KEY
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
  ob2h:
    command: C:\Users\ipres\.cargo\bin\mcp-compressor.exe
    args:
      - -c
      - medium
      - --
      - C:/Projects/omnesbot_for_hermes/.venv/Scripts/python.exe
      - -m
      - ob2h.server
    env:
      OB2H_DATA_DIR: C:/Projects/omnesbot_for_hermes/data
      # ... остальные OB2H_* те же
```

Компрессор полезен для `graph_reason`/`memory_context` с длинным выводом;
для отладки первый запуск лучше делать без него.

## 3. Проверка после подключения

1. Перезапустить Hermes.
2. Убедиться, что инструменты появились (в Hermes — список MCP-инструментов сервера
   `ob2h`).
3. Живой сценарий (из PLAN.md §6.4):
   - попросить Hermes «сохрани в память: …» → `memory_save`;
   - в **новом** чате спросить так, чтобы всплыл факт → `memory_search`;
   - подсунуть документ → `knowledge_extract` → вопрос по содержимому → `graph_reason`;
   - запустить `dream_run` → проверить `workspace/memory/MEMORY.md` и git-историю
     (`dream_log` / `dream_restore`).
4. Логи при проблемах: `C:\Projects\omnesbot_for_hermes\logs\ob2h.log`.

## 4. Ограничения жизненного цикла

- Сервер живёт, пока живёт Hermes (stdio). Фоновый автодрим работает в это же время.
  Если Hermes выключен надолго — дрим можно запустить вручную:
  `.venv\Scripts\python -m ob2h.dream_cli run` (появится в фазе 5, если
  понадобится).
- Все данные — в `OB2H_DATA_DIR`. Перенос на другую машину = скопировать папку
  проекта + `data/` + установить зависимости.
