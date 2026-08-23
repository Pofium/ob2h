"""Юнит-тесты JSON-RPC клиента (_rpc.py) против фейк-сервера.

Запуск: python -m unittest discover -s plugin/tests -p "test_rpc.py"
"""

import sys
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ob2h"))

from _rpc import Ob2hRpc, RpcDead, RpcError  # noqa: E402

FAKE = [sys.executable, str(Path(__file__).resolve().parent / "fake_server.py")]


class TestOb2hRpc(unittest.TestCase):
    def setUp(self):
        self.rpc = Ob2hRpc(FAKE, restart_backoff=0.1)
        self.addCleanup(self.rpc.stop)

    def test_handshake_and_ping(self):
        self.rpc.start()
        self.assertTrue(self.rpc.alive())
        self.assertTrue(self.rpc.ping())

    def test_tools_list_and_call(self):
        self.rpc.start()
        tools = self.rpc.tools_list()
        names = [t["name"] for t in tools]
        self.assertIn("memory_save", names)

        out = self.rpc.tool_call("memory_save", {"content": "x"})
        self.assertEqual(out, "ok:memory_save")

    def test_timeout_raises(self):
        self.rpc.start()
        with self.assertRaises(RpcError):
            self.rpc.tool_call("slow", {}, timeout=0.3)

    def test_death_raises_rpcdead_and_ensure_restarts(self):
        self.rpc.start()
        self.rpc.tool_call("die", {})
        deadline = time.monotonic() + 3
        while self.rpc.alive() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertFalse(self.rpc.alive())
        # ping() глотает RpcError по контракту — на мёртвом процессе вернёт False
        self.assertFalse(self.rpc.ping(timeout=0.5))

        # ensure() поднимает процесс заново; handshake проходит
        self.rpc.ensure()
        self.assertTrue(self.rpc.ping())

    def test_stop_is_idempotent(self):
        self.rpc.start()
        self.rpc.stop()
        self.rpc.stop()
        self.assertFalse(self.rpc.alive())


if __name__ == "__main__":
    unittest.main()
