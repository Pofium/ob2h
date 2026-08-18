"""Фейковые LLM для тестов (AGENTS.md §7): сценарные ответы без сети."""

from __future__ import annotations

from typing import Any


class FakeLLM:
    """Отдаёт запрограммированные ответы по очереди; логирует вызовы."""

    def __init__(self, responses: list[str] | None = None):
        self.responses = list(responses or [])
        self.calls: list[dict] = []

    def chat(self, messages: list[dict[str, str]], *,
             temperature: float = 0.1, max_tokens: int | None = None) -> str:
        self.calls.append({"messages": messages, "temperature": temperature})
        if self.responses:
            return self.responses.pop(0)
        return "заглушка"

    def ask_json(self, system: str, user: str, *,
                 temperature: float = 0.1, max_tokens: int | None = None) -> Any:
        self.calls.append({"system": system, "user": user})
        raw = self.responses.pop(0) if self.responses else "{}"
        import json
        return json.loads(raw)
