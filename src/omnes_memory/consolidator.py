"""Консолидатор сессий по бюджету токенов (порт Consolidator из OmnesBOT).

Механика: пока оценка токенов буфера превышает
(context_window − max_completion − 1024)/2, берём сообщения по границам user-ходов
(≤60 сообщений на раунд, ≤5 раундов), LLM-суммаризуем и аппендим в history.jsonl.
Нет LLM/ошибка — деградированный raw-режим с префиксом [RAW] (порт raw_archive).
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

from .config import Settings
from .llm_client import LLMError, LLMProtocol
from .workspace import Workspace

log = logging.getLogger("omnes.consolidator")

MAX_MESSAGES_PER_ROUND = 60
MAX_ROUNDS = 5

SYSTEM_PROMPT = """\
Ты — архивариус памяти личного агента. Сожми приведённый фрагмент диалога \
в компактный итог для долгосрочной памяти: только факты, решения, пожелания \
пользователя и результаты. Формат — маркированный список на русском, \
каждый пункт самодостаточен. Без вступлений и заголовков."""


def estimate_tokens(text: str) -> int:
    """Грубая оценка токенов (запас в большую сторону для русского)."""
    return max(1, len(text) // 3)


@dataclass
class PendingSession:
    """Буфер текущей сессии между вызовами session_log (in-memory)."""

    messages: list[dict[str, str]] = field(default_factory=list)
    total_estimated: int = 0

    def append(self, role: str, content: str) -> None:
        self.messages.append({"role": role, "content": content})
        self.total_estimated += estimate_tokens(content)


class Consolidator:
    def __init__(self, workspace: Workspace, llm: LLMProtocol | None,
                 settings: Settings):
        self.workspace = workspace
        self.llm = llm
        self.settings = settings

    def budget(self) -> int:
        return (self.settings.context_window - self.settings.max_completion_tokens
                - 1024) // 2

    def maybe_consolidate(self, session: PendingSession) -> dict[str, Any]:
        """Вызывается после каждого хода; консолидирует при переполнении."""
        rounds = 0
        consolidated_entries = 0
        while session.total_estimated > self.budget() and rounds < MAX_ROUNDS:
            batch = self._take_batch(session)
            if not batch:
                break
            summary = self._summarize(batch)
            self.workspace.append_history(summary)
            consolidated_entries += 1
            rounds += 1
        self.workspace.compact_history()
        return {
            "consolidated": consolidated_entries > 0,
            "entries": consolidated_entries,
            "remaining_estimated": session.total_estimated,
        }

    def _take_batch(self, session: PendingSession) -> list[dict[str, str]]:
        """Сообщения по границе user-ходов, ≤60 шт.; продвигает буфер."""
        limit = min(MAX_MESSAGES_PER_ROUND, len(session.messages))
        batch = session.messages[:limit]
        # не рвать пару user→assistant: подрезать до начала user-хода
        while batch and batch[-1]["role"] == "user" and len(batch) < len(session.messages):
            batch.pop()
        if not batch:
            batch = session.messages[:limit]
        consumed = len(batch)
        session.messages = session.messages[consumed:]
        session.total_estimated = sum(
            estimate_tokens(m["content"]) for m in session.messages
        )
        return batch

    def _summarize(self, batch: list[dict[str, str]]) -> str:
        dialogue = "\n".join(
            f"{('Пользователь' if m['role'] == 'user' else 'Агент')}: {m['content']}"
            for m in batch
        )
        if self.llm is None:
            return self._raw_archive(batch)
        try:
            answer = self.llm.chat(
                [{"role": "system", "content": SYSTEM_PROMPT},
                 {"role": "user", "content": dialogue}],
                temperature=0.1,
            )
            return answer.strip() or self._raw_archive(batch)
        except LLMError as e:
            log.warning("LLM-суммаризация не удалась, raw-архив: %s", e)
            return self._raw_archive(batch)

    @staticmethod
    def _raw_archive(batch: list[dict[str, str]]) -> str:
        """Деградированный режим без LLM (порт raw_archive из OmnesBOT)."""
        return "\n".join(f"[RAW] {m['role']}: {m['content'][:500]}" for m in batch)
