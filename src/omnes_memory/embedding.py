"""Эмбеддинги: fastembed (ONNX, CPU, без torch) или OpenAI-совместимый API (ADR-4)."""

from __future__ import annotations

import hashlib
import logging
from typing import Protocol

import httpx
import numpy as np

from .config import Settings

log = logging.getLogger("omnes.embed")


class EmbeddingProvider(Protocol):
    name: str
    dim: int

    def embed(self, texts: list[str]) -> list[np.ndarray]: ...
    def embed_query(self, text: str) -> np.ndarray: ...


class FakeEmbedding:
    """Детерминированная заглушка для тестов: md5-сеяный вектор.

    Детерминизм между запусками процесса (в отличие от hash(), который
    случаен из-за PYTHONHASHSEED) — тесты не флакают.
    """

    def __init__(self, dim: int = 64):
        self.name = "fake"
        self.dim = dim
        self._overrides: dict[str, np.ndarray] = {}

    def set_vector(self, text: str, vector: list[float] | np.ndarray) -> None:
        """Задать вектор конкретному тексту/запросу (контроль в тестах)."""
        v = np.asarray(vector, dtype=np.float32)
        self._overrides[text] = v / (np.linalg.norm(v) + 1e-9)

    def _vec(self, text: str) -> np.ndarray:
        if text in self._overrides:
            return self._overrides[text]
        digest = hashlib.md5(text.encode("utf-8")).digest()  # noqa: S324 — не криптография
        rng = np.random.default_rng(int.from_bytes(digest[:4], "little"))
        v = rng.standard_normal(self.dim).astype(np.float32)
        return v / (np.linalg.norm(v) + 1e-9)

    def embed(self, texts: list[str]) -> list[np.ndarray]:
        return [self._vec(t) for t in texts]

    def embed_query(self, text: str) -> np.ndarray:
        return self._vec(text)


class LocalFastembed:
    """fastembed на CPU. Модель по умолчанию — multilingual-e5-small (384d).

    e5-модели различают запросы и документы — используем query_embed для поиска.
    """

    def __init__(self, model: str):
        self.name = "local"
        try:
            from fastembed import TextEmbedding  # noqa: PLC0415 — опциональная зависимость
        except ImportError as e:  # pragma: no cover - зависит от окружения
            raise RuntimeError(
                "fastembed не установлен. Установите '.[local]' или задайте "
                "OMNES_EMBED_PROVIDER=api (см. docs/ARCHITECTURE.md §6)"
            ) from e
        self._model = TextEmbedding(model_name=model)
        self.name = f"local:{model}"
        self.dim = int(self._probe(text="проба размерности").shape[0])

    def _probe(self, text: str) -> np.ndarray:
        return np.asarray(next(iter(self._model.embed([text]))), dtype=np.float32)

    def embed(self, texts: list[str], batch: int = 32) -> list[np.ndarray]:
        out: list[np.ndarray] = []
        for i in range(0, len(texts), batch):
            for v in self._model.embed(texts[i : i + batch]):
                out.append(np.asarray(v, dtype=np.float32))
        return out

    def embed_query(self, text: str) -> np.ndarray:
        return np.asarray(
            next(iter(self._model.query_embed([text]))), dtype=np.float32
        )


class ApiEmbedding:
    """OpenAI-совместимый /embeddings (aitunnel, openai, совместимые прокси)."""

    def __init__(self, base_url: str, api_key: str, model: str, timeout: float = 60.0):
        self.name = f"api:{model}"
        self._url = base_url.rstrip("/") + "/embeddings"
        self._headers = {"Authorization": f"Bearer {api_key}"} if api_key else {}
        self._model = model
        self._timeout = timeout
        self.dim = self._probe_dim()

    def _probe_dim(self) -> int:
        vec = self._request(["проба размерности"])[0]
        return int(vec.shape[0])

    def _request(self, texts: list[str]) -> list[np.ndarray]:
        resp = httpx.post(
            self._url,
            headers=self._headers,
            json={"model": self._model, "input": texts},
            timeout=self._timeout,
        )
        resp.raise_for_status()
        data = sorted(resp.json()["data"], key=lambda d: d["index"])
        return [np.asarray(d["embedding"], dtype=np.float32) for d in data]

    def embed(self, texts: list[str], batch: int = 32) -> list[np.ndarray]:
        out: list[np.ndarray] = []
        for i in range(0, len(texts), batch):
            out.extend(self._request(texts[i : i + batch]))
        return out

    def embed_query(self, text: str) -> np.ndarray:
        return self._request([text])[0]


def get_embedding_provider(settings: Settings) -> EmbeddingProvider:
    if settings.embed_provider == "fake":  # только для тестов
        return FakeEmbedding()
    if settings.embed_provider == "api":
        return ApiEmbedding(
            base_url=settings.embed_base_url,
            api_key=settings.embed_api_key,
            model=settings.embed_model,
        )
    return LocalFastembed(settings.embed_model)


_cached_provider: EmbeddingProvider | None = None
_cached_provider_key: str | None = None


def provider_for(settings: Settings) -> EmbeddingProvider:
    """Единственный экземпляр провайдера на процесс (модель грузится один раз)."""
    global _cached_provider, _cached_provider_key
    key = (
        f"{settings.embed_provider}|{settings.embed_model}|{settings.embed_base_url}"
    )
    if _cached_provider_key != key:
        _cached_provider = get_embedding_provider(settings)
        _cached_provider_key = key
    return _cached_provider
