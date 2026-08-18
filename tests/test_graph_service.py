"""Тесты графа знаний: дедуп upsert, поиск, reason (фаза 4.4, 4.5)."""

import json

import pytest

from fakes import FakeLLM
from omnes_memory.db import Database
from omnes_memory.embedding import FakeEmbedding
from omnes_memory.extractor import Entity, ExtractionResult, Relation
from omnes_memory.graph_service import GraphService


@pytest.fixture
def graph(tmp_path):
    db = Database(tmp_path / "t.db")
    yield GraphService(db, FakeEmbedding())
    db.close()


def sample_result():
    return ExtractionResult(
        entities=[
            Entity("Иванов", "Person", "инженер"),
            Entity("ООО Ромашка", "Organization", "завод"),
            Entity("Казань", "Location", "город"),
        ],
        relations=[
            Relation("Иванов", "ООО Ромашка", "works_at", ["работает"]),
            Relation("ООО Ромашка", "Казань", "located_in", ["расположен"]),
        ],
    )


def test_upsert_and_dedup(graph):
    s1 = graph.upsert_extraction(sample_result())
    assert s1 == {"new_entities": 3, "updated_entities": 0, "new_edges": 2}

    s2 = graph.upsert_extraction(sample_result())  # повторный прогон — дубли
    assert s2["new_entities"] == 0
    assert s2["updated_entities"] == 3
    assert s2["new_edges"] == 0  # рёбра только инкрементируют weight

    stats = graph.stats()
    assert stats["nodes"] == 3 and stats["edges"] == 2

    row = graph.db.query_one(
        "SELECT gn.val AS val, ge.weight AS weight FROM graph_nodes gn "
        "JOIN graph_edges ge ON ge.source_id = gn.id WHERE gn.label='Иванов'"
    )
    assert row["val"] == 2  # два упоминания
    assert row["weight"] == 2  # ребро инкрементировано, не задублировано


def test_description_concat_on_dedup(graph):
    graph.upsert_extraction(sample_result())
    r2 = ExtractionResult(
        entities=[Entity("Иванов", "Person", "стаж 10 лет")], relations=[])
    graph.upsert_extraction(r2)
    desc = graph.db.query_one(
        "SELECT description FROM graph_nodes WHERE label='Иванов'")["description"]
    assert "инженер" in desc and "стаж 10 лет" in desc


def test_search_scoring_and_neighbors(graph):
    graph.upsert_extraction(sample_result())
    found = graph.search("Иванов")
    labels = {n["label"] for n in found["nodes"]}
    assert "Иванов" in labels
    # 1-hop: сосед по ребру works_at тоже в результатах
    assert "ООО Ромашка" in labels
    edge_labels = {(e["source_label"], e["label"], e["target_label"])
                   for e in found["edges"]}
    assert ("Иванов", "works_at", "ООО Ромашка") in edge_labels


def test_search_empty(graph):
    assert graph.search("ничего") == {"nodes": [], "edges": []}


def test_reason_returns_json_answer(graph):
    graph.upsert_extraction(sample_result())
    llm = FakeLLM(responses=[json.dumps({
        "answer": "Иванов работает в ООО Ромашка",
        "confidence": 0.9,
        "reasoning_steps": ["нашёл сущность", "нашёл отношение"],
        "used_entities": ["Иванов", "ООО Ромашка"],
        "used_relations": ["works_at"],
    }, ensure_ascii=False)])
    answer = graph.reason("Где работает Иванов?", llm)
    assert answer["confidence"] == 0.9
    assert "Ромашка" in answer["answer"]
    assert answer["graph_stats"]["nodes_used"] >= 2


def test_reason_empty_graph(graph):
    answer = graph.reason("что угодно", FakeLLM())
    assert answer["confidence"] == 0.0
    assert "нет данных" in answer["answer"]


def test_reason_llm_error_returns_error_field(graph):
    class Broken:
        def chat(self, *a, **k):
            from omnes_memory.llm_client import LLMError
            raise LLMError("boom")

        def ask_json(self, *a, **k):
            from omnes_memory.llm_client import LLMError
            raise LLMError("boom")

    graph.upsert_extraction(sample_result())
    answer = graph.reason("тест", Broken())
    assert answer["answer"].startswith("[Error]")


def test_node_embeddings_persisted(graph):
    graph.upsert_extraction(sample_result())
    blobs = graph.db.query(
        "SELECT embedding FROM graph_nodes WHERE embedding IS NOT NULL")
    assert len(blobs) == 3
