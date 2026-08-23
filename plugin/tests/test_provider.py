"""Юнит-тесты Ob2hProvider (plugin/ob2h/__init__.py) с фейковым RPC.

Запуск: python -m unittest discover -s plugin/tests -p "test_provider.py"
"""

import importlib.util
import os
import sys
import tempfile
import time
import types
import unittest
from dataclasses import dataclass
from pathlib import Path

PLUGIN_DIR = Path(__file__).resolve().parents[1] / "ob2h"


def _install_abc_stub():
    """Стаб agent.memory_provider вне рантайма Hermes."""
    if "agent.memory_provider" in sys.modules:
        return
    agent = types.ModuleType("agent")

    class MemoryProvider:  # минимальный ABC
        pass

    @dataclass(frozen=True)
    class RecallStatus:
        provider_label: str
        count: int
        glyph: str = "🧠"

    mp = types.ModuleType("agent.memory_provider")
    mp.MemoryProvider = MemoryProvider
    mp.RecallStatus = RecallStatus
    agent.memory_provider = mp
    sys.modules["agent"] = agent
    sys.modules["agent.memory_provider"] = mp


def load_plugin_module():
    """Загрузить __init__.py плагина так же, как это делает лоадер Hermes."""
    _install_abc_stub()
    name = "ob2h_plugin_under_test"
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(
        name, PLUGIN_DIR / "__init__.py", submodule_search_locations=[str(PLUGIN_DIR)]
    )
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    # относительный импорт `from ._rpc import ...` требует зарегистрированного подмодуля
    rpc_spec = importlib.util.spec_from_file_location(f"{name}._rpc", PLUGIN_DIR / "_rpc.py")
    rpc_mod = importlib.util.module_from_spec(rpc_spec)
    sys.modules[f"{name}._rpc"] = rpc_mod
    rpc_spec.loader.exec_module(rpc_mod)
    spec.loader.exec_module(mod)
    return mod


class FakeRpc:
    """Записывает вызовы; отвечает детерминированно."""

    def __init__(self):
        self.calls = []
        self.texts = {
            "memory_context": "<agent_memory>\n- факт раз\n- факт два\n</agent_memory>",
            "memory_search": "[1] key=a | первый\n[2] key=b | второй",
        }

    def ensure(self):
        pass

    def alive(self):
        return True

    def tools_list(self):
        return [
            {"name": "memory_save", "description": "d", "input_schema": {"type": "object"}},
            {"name": "session_log", "description": "d", "input_schema": {"type": "object"}},
            {"name": "memory_context", "description": "d", "input_schema": {"type": "object"}},
            {"name": "graph_reason", "description": "d", "input_schema": {"type": "object"}},
        ]

    def tool_call(self, name, args, timeout=None):
        self.calls.append((name, args))
        return self.texts.get(name, "ok")

    def stop(self):
        pass


def _wait_queue_empty(provider, timeout=3.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if provider._write_q.unfinished_tasks == 0:
            return True
        time.sleep(0.02)
    return False


class TestOb2hProvider(unittest.TestCase):
    def setUp(self):
        self.plugin = load_plugin_module()
        self.fake_bin = tempfile.NamedTemporaryFile(
            suffix=".exe", delete=False
        )
        self.fake_bin.write(b"fake")
        self.fake_bin.close()
        self.addCleanup(os.unlink, self.fake_bin.name)
        self._old_bin = os.environ.get("OB2H_BIN")
        os.environ["OB2H_BIN"] = self.fake_bin.name

        self.provider = self.plugin.Ob2hProvider()
        # initialize с фейковым бинарником: Popen упадёт → деградация (это ок),
        # затем подменяем rpc на фейк.
        self.provider.initialize("sess-a", hermes_home=tempfile.gettempdir(),
                                 agent_context="primary")
        self.fake = FakeRpc()
        self.provider._rpc = self.fake
        self.addCleanup(self.provider.shutdown)

    def tearDown(self):
        if self._old_bin is None:
            os.environ.pop("OB2H_BIN", None)
        else:
            os.environ["OB2H_BIN"] = self._old_bin

    def _ingests(self):
        return [c for c in self.fake.calls if c[0] == "session_ingest"]

    def test_sync_turn_accumulates_full_prefix(self):
        self.provider.sync_turn("вопрос 1", "ответ 1", session_id="sess-a")
        self.assertTrue(_wait_queue_empty(self.provider), "writer не обработал очередь")
        self.provider.sync_turn("вопрос 2", "ответ 2", session_id="sess-a")
        self.assertTrue(_wait_queue_empty(self.provider))

        ingests = self._ingests()
        self.assertEqual(len(ingests), 2)
        first = ingests[0][1]["messages"]
        second = ingests[1][1]["messages"]
        self.assertEqual(len(first), 2)
        self.assertEqual(len(second), 4, "второй ход шлёт полный префикс (для дедупа)")
        self.assertEqual(ingests[0][1]["session_id"], "sess-a")

    def test_non_primary_context_does_not_write(self):
        self.provider._writes_enabled = False
        self.provider.sync_turn("q", "a", session_id="sess-a")
        time.sleep(0.2)
        self.assertEqual(self._ingests(), [])

    def test_session_end_sends_full_transcript(self):
        self.provider.on_session_end([
            {"role": "user", "content": "u1"},
            {"role": "tool", "content": "t"},        # пропускается
            {"role": "assistant", "content": "a1"},
            {"role": "system", "content": "[System]"},  # пропускается
            {"role": "user", "content": "u2"},
            {"role": "assistant", "content": "a2"},
        ])
        self.assertTrue(_wait_queue_empty(self.provider))
        ingest = self._ingests()[0]
        msgs = ingest[1]["messages"]
        self.assertEqual([m["role"] for m in msgs], ["user", "assistant", "user", "assistant"])

    def test_pre_compress_uses_pseudo_session(self):
        self.provider.on_pre_compress([{"role": "user", "content": "u"},
                                       {"role": "assistant", "content": "a"}])
        self.assertTrue(_wait_queue_empty(self.provider))
        name, args = self.fake.calls[-1]
        self.assertEqual(name, "session_ingest")
        self.assertEqual(args["session_id"], "sess-a:precompress")
        self.assertEqual(args["source"], "pre_compress")

    def test_builtin_memory_write_mirrored(self):
        self.provider.on_memory_write("add", "memory", "любимый цвет — синий")
        self.assertTrue(_wait_queue_empty(self.provider))
        saves = [c for c in self.fake.calls if c[0] == "memory_save"]
        self.assertEqual(len(saves), 1)
        self.assertEqual(saves[0][1]["source"], "hermes-builtin")

    def test_tool_schemas_exclude_automatic(self):
        self.provider._refresh_tool_schemas()
        names = [s["name"] for s in self.provider.get_tool_schemas()]
        self.assertEqual(names, ["memory_save", "graph_reason"])

    def test_prefetch_returns_block_and_status(self):
        self.provider.queue_prefetch("что я люблю?", session_id="sess-a")
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            with self.provider._prefetch_lock:
                if "sess-a" in self.provider._prefetch_cache:
                    break
            time.sleep(0.02)
        block = self.provider.prefetch("что я люблю?", session_id="sess-a")
        self.assertIn("факт раз", block)
        status = self.provider.recall_status()
        self.assertEqual(status.count, 2)

    def test_system_prompt_block_static(self):
        self.assertIn("ob2h", self.provider.system_prompt_block())


if __name__ == "__main__":
    unittest.main()
