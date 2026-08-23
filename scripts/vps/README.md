# Синхронизация ob2h на VPS (systemd)

Топология: PC за NAT, поэтому инициатива всегда на PC (push/pull по SSH).
VPS только принимает бандлы в inbox и готовит свои в outbox.

## Установка (однократно, root@vps)

```bash
cp ob2h-sync.service ob2h-sync.timer /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now ob2h-sync.timer
```

## peers.json на VPS (/root/ob2h_data/sync/peers.json)

```json
{
  "origin": "vps",
  "priority": ["pc", "vps"],
  "after_dream": false,
  "peers": {}
}
```

`export --peer pc` использует «pc» только как имя watermark'а — запись в peers не нужна.

## Ручной запуск и проверка

```bash
systemctl start ob2h-sync.service
journalctl -u ob2h-sync.service -n 30
/usr/local/bin/ob2h sync status
```
