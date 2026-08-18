"""Тесты дрима: 2 фазы, агентный цикл правок, курсор, git-коммит (фаза 5.1)."""

import json

import pytest

from fakes import FakeLLM
from omnes_memory.config import Settings
from omnes_memory.db import Database
from omnes_memory.dream import Dream
from omnes_memory.gitstore import GitStore
from omnes_memory.workspace import Workspace


@pytest.fixture
def env(tmp_path):
    ws = Workspace(tmp_path / "ws")
    gs = GitStore(ws.root)
    settings = Settings(_env_file=None, data_dir=tmp_path / "data")
    db = Database(settings.db_path)
    yield ws, gs, settings, db
    db.close()


def prepare_history(ws, n=5):
    for i in range(n):
        ws.append_history(f"итог сессии {i}: пользователь работал над проектом Альфа")


def test_dream_full_cycle_applies_edits(env):
    ws, gs, settings, db = env
    prepare_history(ws)
    ws.write("memory", "# Долгосрочная память\n\n- (пусто — дрим и агент наполнят)\n")
    gs.auto_commit("init")

    llm = FakeLLM(responses=[
        "анализ: стоит добавить факт о проекте Альфа",           # фаза 1
        json.dumps({"action": "edit", "file": "memory",
                    "old": "- (пусто — дрим и агент наполнят)",
                    "new": "- Пользователь ведёт проект Альфа"}, ensure_ascii=False),
        json.dumps({"action": "done", "summary": "добавлен факт"}, ensure_ascii=False),
    ])
    dream = Dream(ws, gs, llm, settings, db)
    result = dream.run(trigger="manual")

    assert result["status"] == "ok"
    assert result["processed"] == 5
    assert result["edits"] == 1
    assert result["commit"]
    assert "проект Альфа" in ws.read("memory")
    assert ws.get_cursor("dream_cursor") == 5  # курсор продвинут

    # git-история содержит dream-коммит
    entries = gs.log()
    assert any(e["message"].startswith("dream:") for e in entries)

    # dream_runs залогирован
    row = db.query_one("SELECT status, trigger FROM dream_runs ORDER BY id DESC")
    assert row["status"] == "ok" and row["trigger"] == "manual"


def test_dream_no_new_records(env):
    ws, gs, settings, db = env
    dream = Dream(ws, gs, FakeLLM(), settings, db)
    result = dream.run()
    assert result["status"] == "ok"
    assert result["processed"] == 0
    assert result.get("note")


def test_dream_requires_llm(env):
    ws, gs, settings, db = env
    prepare_history(ws)
    dream = Dream(ws, gs, None, settings, db)
    result = dream.run()
    assert result["status"] == "error"
    row = db.query_one("SELECT status FROM dream_runs ORDER BY id DESC")
    assert row["status"] == "error"


def test_dream_second_run_processes_only_new(env):
    ws, gs, settings, db = env
    prepare_history(ws, 3)
    done = json.dumps({"action": "done", "summary": "ok"})
    llm = FakeLLM(responses=["анализ", done, "анализ2", done])
    dream = Dream(ws, gs, llm, settings, db)
    dream.run()
    prepare_history(ws, 2)  # записи 4, 5
    result = dream.run()
    assert result["processed"] == 2


def test_dream_edit_retry_on_wrong_fragment(env):
    """LLM промахнулся по фрагменту — получает ошибку и исправляется."""
    ws, gs, settings, db = env
    prepare_history(ws, 2)
    llm = FakeLLM(responses=[
        "анализ",
        json.dumps({"action": "edit", "file": "memory",
                    "old": "такого фрагмента нет", "new": "x"}),      # промах
        json.dumps({"action": "read", "file": "memory"}),             # перечитал
        json.dumps({"action": "edit", "file": "memory",
                    "old": "# Долгосрочная память", "new": "# Память агента"},
                   ensure_ascii=False),
        json.dumps({"action": "done", "summary": "ok"}),
    ])
    dream = Dream(ws, gs, llm, settings, db)
    result = dream.run()
    assert result["edits"] == 1
    assert ws.read("memory").startswith("# Память агента")


def test_dream_batch_limit(env):
    ws, gs, settings, db = env
    prepare_history(ws, 50)
    llm = FakeLLM(responses=["анализ", json.dumps({"action": "done", "summary": "s"})])
    settings_small = Settings(_env_file=None, data_dir=settings.data_dir,
                              dream_batch=20)
    result = Dream(ws, gs, llm, settings_small, db).run()
    assert result["processed"] == 20  # батч 20 (порт из OmnesBOT)


def test_dream_restore_via_git(env):
    ws, gs, settings, db = env
    prepare_history(ws)
    gs.auto_commit("init")  # исходное состояние до дрима
    llm = FakeLLM(responses=[
        "анализ",
        json.dumps({"action": "edit", "file": "user", "old": "# USER",
                    "new": "# USER\n- Владелец любит кофе"}, ensure_ascii=False),
        json.dumps({"action": "done", "summary": "ok"}),
    ])
    Dream(ws, gs, llm, settings, db).run()
    assert "кофе" in ws.read("user")
    first = gs.log()[-1]["sha"]  # самый старый коммит (до дрима)
    gs.restore(first)
    assert "кофе" not in ws.read("user")
