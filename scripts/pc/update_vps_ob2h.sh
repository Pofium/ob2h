#!/usr/bin/env bash
# Разовое обновление ob2h на VPS Hermes: установка свежесобранного бинарника.
# Использование: bash scripts/pc/update_vps_ob2h.sh
# (сборка к этому моменту должна быть завершена на MSYS path conversion —
#  Git Bash конвертирует /root/... в аргументах при вызове python.exe,
#  поэтому MSYS_NO_PATHCONV=1 и Windows-пути для локальных файлов)
set -e
export MSYS_NO_PATHCONV=1
PY="C:/Program Files/Python313/python.exe"
BRIDGE="C:/Users/ipres/hermes/vps_bridge.py"

# 1) остановить работающий сервер (watchdog Hermes перезапустит по требованию)
"$PY" "$BRIDGE" exec "pkill -f 'ob2h serve' || true"

# 2) установить новый бинарник
"$PY" "$BRIDGE" exec "install -m 755 /root/build/ob2h/target/release/ob2h /usr/local/bin/ob2h"

# 3) проверить версию установленного
"$PY" "$BRIDGE" exec "/usr/local/bin/ob2h --version && echo INSTALL_OK"
