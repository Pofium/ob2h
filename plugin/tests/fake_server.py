"""Фейковый JSON-RPC сервер для тестов _rpc-клиента (запускается как скрипт).

Читает строки JSON-RPC из stdin, отвечает в stdout. Специальные tools/call:
- "slow"  — спит 2с (для теста таймаута);
- "die"   — завершает процесс (для теста рестарта).
"""

import json
import sys
import time

TOOLS = [
    {"name": "memory_save", "description": "d", "input_schema": {"type": "object"}},
    {"name": "memory_context", "description": "d", "inputSchema": {"type": "object"}},
    {"name": "session_ingest", "description": "d", "inputSchema": {"type": "object"}},
]


def reply(mid, result=None, error=None):
    msg = {"jsonrpc": "2.0", "id": mid}
    if error is not None:
        msg["error"] = error
    else:
        msg["result"] = result if result is not None else {}
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        method = req.get("method", "")
        mid = req.get("id")
        if method == "initialize":
            reply(mid, {"protocolVersion": "2024-11-05", "serverInfo": {"name": "fake"}})
        elif method in ("notifications/initialized", "initialized"):
            pass
        elif method == "ping":
            reply(mid, {})
        elif method == "tools/list":
            reply(mid, {"tools": TOOLS})
        elif method == "tools/call":
            name = (req.get("params") or {}).get("name", "")
            if name == "slow":
                time.sleep(2.0)
                reply(mid, {"content": [{"type": "text", "text": "late"}]})
            elif name == "die":
                reply(mid, {"content": [{"type": "text", "text": "bye"}]})
                sys.exit(0)
            elif name == "memory_context":
                reply(mid, {"content": [{"type": "text",
                                          "text": "<agent_memory>\n- факт\n</agent_memory>"}]})
            else:
                reply(mid, {"content": [{"type": "text",
                                          "text": f"ok:{name}"}]})
        elif mid is not None:
            reply(mid, error={"code": -32601, "message": f"unknown {method}"})


if __name__ == "__main__":
    main()
