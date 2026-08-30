"""Интеграционный тест: плагин-RPC против реального `ob2h serve`.

Пропускается, если бинарник не собран (cargo build / cargo test создают
target/debug/ob2h.exe). Запуск: python -m unittest discover -s plugin/tests -p "test_integration.py"
"""

import sys
import tempfile
import time
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "plugin" / "ob2h"))

from _rpc import Ob2hRpc  # noqa: E402

CANDIDATES = [
    # debug свежее после cargo build/test; release может быть залочен живым Hermes
    REPO / "target" / "debug" / "ob2h.exe",
    REPO / "target" / "release" / "ob2h.exe",
    REPO / "target" / "debug" / "ob2h",
    REPO / "target" / "release" / "ob2h",
]
BINARY = next((p for p in CANDIDATES if p.is_file()), None)


@unittest.skipUnless(BINARY, "ob2h binary not built (cargo build)")
class TestRealServer(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.rpc = Ob2hRpc(
            [str(BINARY), "serve"],
            env={"OB2H_DATA_DIR": self.tmp.name},
            restart_backoff=0.1,
        )
        self.rpc.start()
        self.addCleanup(self.rpc.stop)

    def test_handshake_and_contract(self):
        tools = self.rpc.tools_list()
        names = [t["name"] for t in tools]
        # контракт v1.0: 24 инструмента (19 базовых + 5 проектных)
        self.assertEqual(names[-1], "project_report")
        self.assertEqual(len(names), 24)

    def test_turn_lands_in_daily_log(self):
        out = self.rpc.tool_call(
            "session_ingest",
            {
                "messages": [
                    {"role": "user", "content": "Привет, память"},
                    {"role": "assistant", "content": "Привет! Записал."},
                ],
                "session_id": "integ-1",
            },
        )
        self.assertIn("ingested pairs=1", out)

        daily = Path(self.tmp.name) / "workspace" / "daily"
        files = list(daily.glob("*.jsonl"))
        self.assertTrue(files, "daily-лог не создан")
        content = files[0].read_text(encoding="utf-8")
        self.assertIn("Привет, память", content)

    def test_memory_roundtrip(self):
        self.rpc.tool_call(
            "memory_save",
            {"content": "интеграционный факт про ob2h", "importance": 0.9},
        )
        ctx = self.rpc.tool_call("memory_context", {"query": "ob2h"})
        # индексация мгновенная (FTS), вектор — локальная модель; даём шанс
        found = False
        for _ in range(20):
            ctx = self.rpc.tool_call("memory_context", {"query": "ob2h"})
            if "интеграционный факт" in ctx:
                found = True
                break
            time.sleep(0.2)
        self.assertTrue(found, f"факт не всплыл в memory_context: {ctx[:200]}")


if __name__ == "__main__":
    unittest.main()
