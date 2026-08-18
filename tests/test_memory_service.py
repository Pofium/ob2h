"""Тесты сервиса памяти: FTS, вектор, гибрид RRF, важность, контекст (фаза 2.1)."""

import pytest

from omnes_memory.db import Database
from omnes_memory.embedding import FakeEmbedding
from omnes_memory.memory_service import MemoryService


@pytest.fixture
def svc(tmp_path):
    db = Database(tmp_path / "t.db")
    yield MemoryService(db, FakeEmbedding())
    db.close()


def test_upsert_create_and_update(svc):
    r1 = svc.upsert("Пользователь пьёт кофе по утрам", key="coffee")
    assert r1 == {"key": "coffee", "status": "created"}
    r2 = svc.upsert("Пользователь пьёт чай по утрам", key="coffee")
    assert r2["status"] == "updated"
    assert svc.get("coffee")["content"].startswith("Пользователь пьёт чай")


def test_search_fts_russian(svc):
    svc.upsert("Сервер OmnesBOT работает на FastAPI", key="a")
    svc.upsert("Любимый напиток — кофе с молоком", key="b")
    hits = svc.search_fts("кофе")
    assert len(hits) == 1 and hits[0]["key"] == "b"


def test_search_fts_too_short_query(svc):
    svc.upsert("тест", key="a")
    assert svc.search_fts("аб") == []


def test_search_vector(svc):
    # у FakeEmbedding случайные вектора — проверяем механику на одинаковых текстах
    svc.upsert("одна и та же строка", key="x")
    hits = svc.search_vector("одна и та же строка")
    assert hits and hits[0]["key"] == "x"


def test_search_hybrid_rrf_promotes_both(svc):
    svc.upsert("котёл паровой установлен в котельной", key="k1")
    svc.upsert("котёл требует замены прокладок", key="k2")
    svc.upsert("погода солнечная", key="k3")
    hits = svc.search_hybrid("котёл")
    keys = [h["key"] for h in hits]
    # k1/k2 найдены обеими ветками: fts-вклад гарантирует им строгое преимущество
    # над чисто векторными кандидатами (случайные вектора FakeEmbedding)
    assert set(keys[:2]) == {"k1", "k2"}
    assert all("rrf_score" in h for h in hits)


def test_hybrid_rrf_formula(svc):
    """Проверка формулы: запись, найденная обеими ветками, получает сумму RRF."""
    svc.upsert("уникальный технический термин зузуц", key="m1")
    # одна запись: fts rank0 + vector rank0 = 2/(60+1)
    hits = svc.search_hybrid("уникальный технический термин зузуц")
    assert abs(hits[0]["rrf_score"] - 2 / 61) < 1e-4


def test_decay_and_purge(svc):
    svc.upsert("неважное", key="w", importance=0.5)
    svc.db.execute("UPDATE memories SET access_count=0 WHERE key='w'")

    def imp() -> float:
        return svc.db.query_one("SELECT importance FROM memories WHERE key='w'")["importance"]

    svc.decay_importance(rate=0.5)
    assert imp() == pytest.approx(0.25)
    svc.decay_importance(rate=0.5)
    assert imp() == pytest.approx(0.125)
    svc.decay_importance(rate=0.5)
    assert imp() == pytest.approx(0.0625)
    svc.decay_importance(rate=0.5)  # клампится к минимуму
    assert imp() == pytest.approx(0.05)
    assert svc.purge_weak(threshold=0.06) == 1
    assert svc.db.query_one("SELECT 1 FROM memories WHERE key='w'") is None


def test_build_context_block(svc):
    svc.upsert("важный факт о работе", key="a", importance=0.9)
    svc.upsert("маловажный факт", key="b", importance=0.1)
    block = svc.build_context(query="работа")
    assert block.startswith("<agent_memory>") and block.endswith("</agent_memory>")
    assert "важный факт о работе" in block


def test_get_bumps_access(svc):
    svc.upsert("факт", key="a")
    svc.get("a"), svc.get("a")
    assert svc.get("a")["access_count"] == 2


def test_forget(svc):
    svc.upsert("факт", key="a")
    assert svc.forget("a") == "deleted"
    assert svc.forget("a") == "not_found"
