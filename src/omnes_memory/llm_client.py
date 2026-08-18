"""Тонкий OpenAI-совместимый LLM-клиент: ретраи, таймауты, JSON-парсер.

Единственный модуль проекта, который ходит в сеть за LLM (AGENTS.md §6).
Все остальные (consolidator, extractor, dream, graph reason) — только через него.
"""

from __future__ import annotations

import json
import logging
import re
import time
from typing import Any, Protocol

import httpx

from .config import Settings

log = logging.getLogger("omnes.llm")

_FENCE_RE = re.compile(r"^```(?:json)?\s*|\s*```$", re.MULTILINE)


class LLMProtocol(Protocol):
    """Интерфейс для FakeLLM в тестах и реального клиента."""

    def chat(
        self, messages: list[dict[str, str]], *,
        temperature: float = 0.1, max_tokens: int | None = None,
    ) -> str: ...

    def ask_json(
        self, system: str, user: str, *,
        temperature: float = 0.1, max_tokens: int | None = None,
    ) -> Any: ...


class LLMError(RuntimeError):
    pass


class LLMClient:
    def __init__(
        self, base_url: str, api_key: str, model: str,
        timeout: float = 120.0, max_retries: int = 3,
    ):
        if not api_key:
            raise LLMError(
                "Не задан OMNES_LLM_API_KEY — LLM-функции недоступны "
                "(консолидатор перейдёт в raw-режим)"
            )
        self._url = base_url.rstrip("/") + "/chat/completions"
        self._headers = {"Authorization": f"Bearer {api_key}"}
        self.model = model
        self._timeout = timeout
        self._max_retries = max_retries

    def chat(
        self, messages: list[dict[str, str]], *,
        temperature: float = 0.1, max_tokens: int | None = None,
    ) -> str:
        payload: dict[str, Any] = {
            "model": self.model,
            "messages": messages,
            "temperature": temperature,
        }
        if max_tokens:
            payload["max_tokens"] = max_tokens
        last_error = ""
        for attempt in range(self._max_retries):
            try:
                resp = httpx.post(
                    self._url, headers=self._headers, json=payload,
                    timeout=self._timeout,
                )
                if resp.status_code >= 500:
                    last_error = f"HTTP {resp.status_code}"
                elif resp.status_code >= 400:
                    body = resp.text[:300]
                    raise LLMError(f"HTTP {resp.status_code}: {body}")  # не ретраим 4xx
                else:
                    return resp.json()["choices"][0]["message"]["content"] or ""
            except httpx.HTTPError as e:
                last_error = str(e)
            time.sleep(min(2**attempt, 8))
        raise LLMError(f"LLM недоступен после {self._max_retries} попыток: {last_error}")

    def ask_json(
        self, system: str, user: str, *,
        temperature: float = 0.1, max_tokens: int | None = None,
    ) -> Any:
        raw = self.chat(
            [{"role": "system", "content": system},
             {"role": "user", "content": user}],
            temperature=temperature, max_tokens=max_tokens,
        )
        return parse_json_loose(raw)


def parse_json_loose(raw: str) -> Any:
    """JSON из ответа LLM: срезает markdown-обёртки, ищет первый блок {...}/[...]."""
    text = raw.strip()
    if text.startswith("```"):
        text = _FENCE_RE.sub("", text).strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    # поиск первого сбалансированного объекта/массива
    for open_ch, close_ch in (("{", "}"), ("[", "]")):
        start = text.find(open_ch)
        if start == -1:
            continue
        depth = 0
        for i, ch in enumerate(text[start:], start=start):
            if ch == open_ch:
                depth += 1
            elif ch == close_ch:
                depth -= 1
                if depth == 0:
                    try:
                        return json.loads(text[start : i + 1])
                    except json.JSONDecodeError:
                        break
        continue
    raise LLMError(f"LLM вернул не-JSON: {raw[:200]!r}")


def make_llm(settings: Settings) -> LLMProtocol | None:
    """Клиент или None, если ключа нет (деградированный raw-режим)."""
    try:
        return LLMClient(
            base_url=settings.llm_base_url,
            api_key=settings.llm_api_key,
            model=settings.llm_model,
            timeout=settings.llm_timeout,
            max_retries=settings.llm_max_retries,
        )
    except LLMError as e:
        log.warning("LLM отключён: %s", e)
        return None
