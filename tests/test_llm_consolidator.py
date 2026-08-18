"""Тесты LLM-клиента и консолидатора (фаза 3). Без сети: FakeLLM."""

import pytest

from fakes import FakeLLM
from omnes_memory.config import Settings
from omnes_memory.consolidator import Consolidator, PendingSession, estimate_tokens
from omnes_memory.llm_client import LLMClient, LLMError, parse_json_loose
from omnes_memory.workspace import Workspace

# --- parse_json_loose ---


def test_parse_json_loose_plain():
    assert parse_json_loose('{"a": 1}') == {"a": 1}


def test_parse_json_loose_fenced():
    raw = "Вот ответ:\n```json\n{\"entities\": [1, 2]}\n```\nконец"
    assert parse_json_loose(raw) == {"entities": [1, 2]}


def test_parse_json_loose_embedded():
    raw = 'результат: {"x": "внутри", "y": 2} — готово'
    assert parse_json_loose(raw) == {"x": "внутри", "y": 2}


def test_parse_json_loose_garbage_raises():
    with pytest.raises(LLMError):
        parse_json_loose("совсем не json")


# --- LLMClient: без ключа и 4xx без ретраев ---


def test_llm_client_requires_key():
    with pytest.raises(LLMError):
        LLMClient("https://x", "", "m")


def test_llm_client_4xx_no_retry(monkeypatch):
    calls = []

    class FakeResp:
        status_code = 401
        text = "unauthorized"

    def fake_post(url, **kw):
        calls.append(1)
        return FakeResp()

    import omnes_memory.llm_client as lc
    monkeypatch.setattr(lc.httpx, "post", fake_post)
    client = lc.LLMClient("https://x", "key", "m", max_retries=3)
    with pytest.raises(LLMError):
        client.chat([{"role": "user", "content": "hi"}])
    assert len(calls) == 1  # 4xx не ретраится


# --- Консолидатор ---


@pytest.fixture
def env(tmp_path):
    ws = Workspace(tmp_path / "ws")
    settings = Settings(_env_file=None, data_dir=tmp_path / "data",
                        context_window=1000, max_completion_tokens=128)
    return ws, settings


def make_settings(tmp_path, context_window):
    return Settings(_env_file=None, data_dir=tmp_path / "data",
                    context_window=context_window, max_completion_tokens=128)


def test_budget_formula(env):
    ws, settings = env
    c = Consolidator(ws, None, settings)
    assert c.budget() == (1000 - 128 - 1024) // 2


def test_consolidate_triggers_on_overflow_with_llm(env):
    ws, settings = env
    llm = FakeLLM(responses=["- факт раз\n- факт два"])
    c = Consolidator(ws, llm, settings)
    session = PendingSession()
    session.append("user", "расскажи всё " * 300)     # ~4500 симв → > бюджета
    session.append("assistant", "ответ " * 100)
    result = c.maybe_consolidate(session)
    assert result["consolidated"]
    history = ws.load_history()
    assert any("факт раз" in r["content"] for r in history)
    assert session.total_estimated <= c.budget() or not session.messages


def test_consolidate_raw_fallback_without_llm(env):
    ws, settings = env
    c = Consolidator(ws, None, settings)  # LLM нет — raw-режим
    session = PendingSession()
    session.append("user", "важный разговор " * 200)
    session.append("assistant", "итог " * 100)
    result = c.maybe_consolidate(session)
    assert result["consolidated"]
    history = ws.load_history()
    assert any(r["content"].startswith("[RAW]") for r in history)


def test_consolidate_llm_error_falls_back_to_raw(env):
    class BrokenLLM:
        def chat(self, *a, **kw):
            raise LLMError("boom")

        def ask_json(self, *a, **kw):
            raise LLMError("boom")

    ws, settings = env
    c = Consolidator(ws, BrokenLLM(), settings)
    session = PendingSession()
    session.append("user", "текст " * 400)
    session.append("assistant", "ответ")
    c.maybe_consolidate(session)
    assert any(r["content"].startswith("[RAW]") for r in ws.load_history())


def test_no_consolidation_below_budget(tmp_path):
    ws = Workspace(tmp_path / "ws")
    llm = FakeLLM()
    c = Consolidator(ws, llm, make_settings(tmp_path, context_window=8000))
    session = PendingSession()
    session.append("user", "короткий вопрос")
    session.append("assistant", "короткий ответ")
    result = c.maybe_consolidate(session)
    assert not result["consolidated"]
    assert llm.calls == []
    assert ws.load_history() == []


def test_batch_respects_user_turn_boundaries(env):
    ws, settings = env
    llm = FakeLLM(responses=["- итог"] * 5)
    c = Consolidator(ws, llm, settings)
    session = PendingSession()
    # 3 полных хода; консолидация не должна рвать пару user→assistant
    for i in range(3):
        session.append("user", f"вопрос {i} " + "x" * 600)
        session.append("assistant", f"ответ {i} " + "y" * 600)
    c.maybe_consolidate(session)
    for call in llm.calls:
        dialogue = call["messages"][1]["content"]
        assert dialogue.count("Пользователь:") == dialogue.count("Агент:")


def test_estimate_tokens_positive():
    assert estimate_tokens("") >= 1
    assert estimate_tokens("abcdef") == 2
