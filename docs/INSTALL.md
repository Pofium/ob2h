# Установка и жизненный цикл OB2H v2.0.0

## 1. Установка

### Windows 11
- Скачайте `ob2h-v2.0.0-Windows-x86_64-msvc.zip` из GitHub Releases / GitVerse (включает GUI `viewer` и `ob2h.exe`) или используйте Inno Setup инсталлятор (`packaging/ob2h.iss`).
- Добавьте каталог с `ob2h.exe` в `PATH` или выполните `ob2h doctor --fix`.

### Linux (x86_64 gnu / musl)
- Скачайте `ob2h-v2.0.0-Linux-x86_64-musl.tar.gz` (статический бинарник без системных зависимостей) или `Linux-x86_64-gnu.tar.gz`, проверьте контрольную сумму `SHA256SUMS-*.txt`.
- Распакуйте в `~/.local/bin/ob2h`.

### macOS (Apple Silicon / Intel)
- Скачайте `ob2h-v2.0.0-macOS-aarch64.tar.gz` или `macOS-x86_64.tar.gz`.
- **Gatekeeper (если бинарник не нотаризован):**
  ```bash
  xattr -d com.apple.quarantine ./ob2h
  ```

## 2. Настройка IDE и фонового сервиса
- Автоподключение агентов (JSONC-safe с бэкапом): `ob2h agent install --agent all`
- Фоновый пользовательский демон (Task Scheduler / launchd / systemd --user): `ob2h service install`
- Shell completions: `ob2h completions powershell|bash|zsh|fish`
- Предзагрузка моделей (опционально): `ob2h models hydrate`

## 3. Диагностика и миграции
- Pre-flight и автоисправление: `ob2h doctor --fix`
- Предпросмотр миграции БД с авто-бэкапом: `ob2h migrate --dry-run` и `ob2h migrate`

## 4. Удаление
- С сохранением данных (`data_dir`): `ob2h self uninstall --keep-data`
- Полное удаление: `ob2h self uninstall --purge-data`
- Тихий режим (CI / Inno Setup): `--silent`
