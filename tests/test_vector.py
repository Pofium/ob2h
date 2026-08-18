"""Тесты векторного слоя: сериализация и косинусный top-k (фаза 1.4)."""

import numpy as np

from omnes_memory.vector import cosine, deserialize, serialize, top_k


def test_serialize_deserialize_roundtrip():
    vec = np.array([0.1, -0.2, 0.3], dtype=np.float32)
    restored = deserialize(serialize(vec))
    assert np.allclose(restored, vec)
    assert deserialize(None) is None


def test_serialize_stable_dtype():
    blob = serialize([1.0, 2.0])  # list тоже принимается
    assert np.frombuffer(blob, dtype=np.float32).tolist() == [1.0, 2.0]


def test_cosine_bounds():
    a = np.array([1.0, 0.0], dtype=np.float32)
    assert abs(cosine(a, np.array([1.0, 0.0])) - 1.0) < 1e-6
    assert abs(cosine(a, np.array([0.0, 1.0]))) < 1e-6
    assert cosine(a, np.zeros(2)) == 0.0


def test_top_k_ordering_and_filtering():
    q = np.array([1.0, 0.0], dtype=np.float32)
    cands = [
        (1, serialize(np.array([0.9, 0.1]))),
        (2, serialize(np.array([0.0, 1.0]))),   # ортогонален
        (3, serialize(np.array([1.0, 0.0]))),
        (4, None),                               # без вектора
    ]
    result = top_k(q, cands, k=2, min_score=0.5)
    assert [cid for cid, _ in result] == [3, 1]  # точное совпадение — первый
    assert result[0][1] >= result[1][1]


def test_top_k_dim_mismatch_skipped():
    q = np.zeros(4, dtype=np.float32)
    cands = [(1, serialize(np.zeros(8)))]
    assert top_k(q, cands, k=5) == []


def test_fake_embedding_deterministic():
    from omnes_memory.embedding import FakeEmbedding
    f = FakeEmbedding(dim=16)
    v1, v2 = f.embed(["текст"]), f.embed(["текст"])
    assert np.allclose(v1[0], v2[0])
    assert f.embed_query("текст").shape == (16,)
