"""Тесты AutoDreamWorker: гейты, lock, ретеншн (фазы 5.2, 6.3)."""

from datetime import UTC, datetime, timedelta

import pytest

from fakes import FakeLLM
from omnes_memory.autodream import AutoDreamWorker
from omnes_memory.config import Settings
from omnes_memory.dream import Dream
from omnes_memory.gitstore import GitStore
from omnes_memory.workspace import Workspace


@pytest.fixture
def env(tmp_path):
    settings = Settings(_env_file=None, data_dir=tmp_path / "data",
                        autodream_min_interval_h=4, autodream_min_events=10,
                        retention_days=30)
    ws = Workspace(tmp_path / "ws")
    gs = GitStore(ws.root)
    return ws, gs, settings


class FakeClock:
    def __init__(self, start: datetime):
        self.now = start

    def __call__(self) -> datetime:
        return self.now


def make_worker(ws, gs, settings, clock, llm=None) -> AutoDreamWorker:

    class _Db:
        def execute(self, *a, **k):
            class _Cur:
                lastrowid = 1

            return _Cur()

        def query_one(self, *a, **k):
            return None

    dream = Dream(ws, gs, llm or FakeLLM(), settings, _Db())  # type: ignore[arg-type]
    return AutoDreamWorker(dream, ws, settings, clock=clock)


def add_events(ws, n, ts="2026-08-18T10:00:00+00:00"):
    for i in range(n):
        ws.append_daily_event({"timestamp": ts, "query": f"q{i}", "answer_preview": "a"})


def test_gate_no_state_runs_when_events(env):
    ws, gs, settings = env
    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = make_worker(ws, gs, settings, clock)
    add_events(ws, 10)
    assert worker.should_run() == (True, "ok")


def test_gate_too_few_events(env):
    ws, gs, settings = env
    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = make_worker(ws, gs, settings, clock)
    add_events(ws, 3)
    ok, reason = worker.should_run()
    assert not ok and "3 < 10" in reason


def test_gate_too_soon_after_last_run(env):
    ws, gs, settings = env
    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = make_worker(ws, gs, settings, clock)
    worker._save_last_run()
    add_events(ws, 50)
    clock.now += timedelta(hours=1)  # всего 1ч с прошлого запуска
    ok, reason = worker.should_run()
    assert not ok and "1.0ч < 4" in reason


def test_gate_passes_after_interval(env):
    ws, gs, settings = env
    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = make_worker(ws, gs, settings, clock)
    worker._save_last_run()
    clock.now += timedelta(hours=1)
    add_events(ws, 15, ts="2026-08-18T13:00:00+00:00")  # после прошлого запуска
    clock.now += timedelta(hours=4)
    assert worker.should_run() == (True, "ok")


def test_gate_counts_only_new_events(env):
    ws, gs, settings = env
    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = make_worker(ws, gs, settings, clock)
    add_events(ws, 5, ts="2026-08-17T10:00:00+00:00")  # до прошлого запуска
    worker._save_last_run()  # last_run = 2026-08-18T12:00
    add_events(ws, 4, ts="2026-08-18T13:00:00+00:00")  # после — только они считаются
    clock.now += timedelta(hours=10)
    ok, reason = worker.should_run()
    assert not ok and "4 < 10" in reason


def test_lock_acquire_release_and_stale(env):
    ws, gs, settings = env
    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = make_worker(ws, gs, settings, clock)
    assert worker._acquire_lock()
    assert not worker._acquire_lock()  # второй не проходит
    worker._release_lock()
    assert worker._acquire_lock()

    # stale: mtime в прошлом > 1ч — перехват
    import os
    import time as time_mod

    worker._acquire_lock()
    old = time_mod.time() - 7200
    os.utime(worker.lock_file, (old, old))
    assert worker._acquire_lock()


def test_prune_daily_removes_old_files(env, tmp_path):
    ws, gs, settings = env
    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = make_worker(ws, gs, settings, clock)
    (ws.root / "daily" / "2026-07-01.jsonl").write_text("{}", encoding="utf-8")
    (ws.root / "daily" / "2026-08-17.jsonl").write_text("{}", encoding="utf-8")
    removed = worker.prune_daily()
    assert removed == 1
    assert not (ws.root / "daily" / "2026-07-01.jsonl").exists()
    assert (ws.root / "daily" / "2026-08-17.jsonl").exists()


def test_worker_loop_runs_dream_once(env):
    """Полный цикл потока: гейты прошли → дрим выполнен → last_run обновлён."""
    import threading


    ws, gs, settings = env
    done = threading.Event()
    calls = []

    class RecordingDream:
        def run(self, trigger="manual"):
            calls.append(trigger)
            done.set()
            return {"status": "ok", "run_id": 1, "processed": 1, "edits": 0,
                    "commit": None}

    clock = FakeClock(datetime(2026, 8, 18, 12, 0, tzinfo=UTC))
    worker = AutoDreamWorker(RecordingDream(), ws, settings, clock=clock)  # type: ignore[arg-type]
    add_events(ws, 12)
    worker._stop.set()  # первый wait(interval) вернёт True сразу? нет — wait с
    # установленным событием вернёт True немедленно и цикл выйдет, не отработав.
    worker._stop.clear()

    def wait_once(timeout=None):
        # первая итерация — «проснулись, работаем»; дальше — стоп
        return bool(calls)

    worker._stop.wait = wait_once  # type: ignore[method-assign]
    worker.run()
    assert calls == ["auto"]
    assert worker.last_run_iso() is not None
