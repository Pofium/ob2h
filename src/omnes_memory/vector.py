"""Вектора: сериализация BLOB float32 и косинусный поиск перебором (ADR-2)."""

from __future__ import annotations

import numpy as np


def serialize(vec: np.ndarray | list[float]) -> bytes:
    arr = np.asarray(vec, dtype=np.float32)
    return arr.tobytes()


def deserialize(blob: bytes | None) -> np.ndarray | None:
    if blob is None:
        return None
    return np.frombuffer(blob, dtype=np.float32)


def normalize(vec: np.ndarray) -> np.ndarray:
    norm = float(np.linalg.norm(vec))
    if norm == 0.0:
        return vec
    return vec / norm


def cosine(a: np.ndarray, b: np.ndarray) -> float:
    denom = float(np.linalg.norm(a)) * float(np.linalg.norm(b))
    if denom == 0.0:
        return 0.0
    return float(np.dot(a, b) / denom)


def top_k(
    query: np.ndarray,
    candidates: list[tuple[int, bytes | None]],
    k: int,
    min_score: float = 0.0,
) -> list[tuple[int, float]]:
    """Косинусный поиск перебором. candidates: [(id, blob)], возвращает [(id, score)]."""
    scored: list[tuple[int, float]] = []
    q = np.asarray(query, dtype=np.float32)
    for cid, blob in candidates:
        vec = deserialize(blob)
        if vec is None or vec.shape != q.shape:
            continue
        score = cosine(q, vec)
        if score >= min_score:
            scored.append((cid, score))
    scored.sort(key=lambda x: x[1], reverse=True)
    return scored[:k]
