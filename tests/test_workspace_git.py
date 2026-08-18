"""Тесты workspace (порт MemoryStore) и gitstore (фазы 2.2, 2.3)."""

import pytest

from omnes_memory.gitstore import GitStore
from omnes_memory.workspace import Workspace


@pytest.fixture
def ws(tmp_path):
    return Workspace(tmp_path / "workspace")


class TestWorkspace:
    def test_defaults_created(self, ws):
        assert (ws.root / "memory" / "MEMORY.md").exists()
        assert (ws.root / "SOUL.md").exists()
        assert (ws.root / "USER.md").exists()

    def test_read_write_md(self, ws):
        ws.write("memory", "# Память\n- факт один\n")
        assert "факт один" in ws.read("memory")

    def test_unknown_file_rejected(self, ws):
        with pytest.raises(ValueError):
            ws.read("unknown")
        with pytest.raises(ValueError):
            ws.write("history", "...")  # history — только через append_history

    def test_history_append_load_compact(self, ws):
        ws.append_history("итог сессии 1")
        rec = ws.append_history("итог сессии 2")
        assert rec["cursor"] == 2
        assert len(ws.load_history()) == 2
        assert ws.compact_history(max_items=1) == 1
        assert len(ws.load_history()) == 1

    def test_history_tolerates_corrupt_lines(self, ws):
        ws.append_history("ok")
        ws.history_path.write_text(
            "{битая строка\n" + ws.history_path.read_text(encoding="utf-8"),
            encoding="utf-8",
        )
        assert len(ws.load_history()) == 1

    def test_cursors(self, ws):
        assert ws.get_cursor("dream_cursor") == 0
        ws.set_cursor("dream_cursor", 42)
        assert ws.get_cursor("dream_cursor") == 42

    def test_daily_events(self, ws):
        ws.append_daily_event({"timestamp": "2026-08-18T10:00:00+00:00",
                               "query": "вопрос", "answer_preview": "ответ"})
        events = ws.load_daily_events()
        assert events[0]["query"] == "вопрос"
        assert ws.count_daily_events_since("2026-08-18T09:00:00+00:00") == 1
        assert ws.count_daily_events_since("2026-08-18T11:00:00+00:00") == 0


class TestGitStore:
    def test_commit_log_restore_cycle(self, tmp_path):
        ws = Workspace(tmp_path / "workspace")
        gs = GitStore(ws.root)
        ws.write("memory", "версия 1")
        sha1 = gs.auto_commit("dream: первый")
        assert sha1
        ws.write("memory", "версия 2")
        sha2 = gs.auto_commit("dream: второй")
        assert sha2 and sha1 != sha2

        entries = gs.log()
        assert len(entries) >= 2
        assert entries[0]["message"] == "dream: второй"

        assert gs.restore(entries[-1]["sha"]) .startswith("restored")
        assert ws.read("memory") == "версия 1"

    def test_auto_commit_no_changes(self, tmp_path):
        ws = Workspace(tmp_path / "workspace")
        gs = GitStore(ws.root)
        gs.auto_commit("первый коммит")
        assert gs.auto_commit("без изменений") is None

    def test_local_identity_only(self, tmp_path):
        ws = Workspace(tmp_path / "workspace")
        GitStore(ws.root).ensure_repo()
        cfg = (ws.root / ".git" / "config").read_text(encoding="utf-8")
        assert "omnes-dream@local" in cfg
