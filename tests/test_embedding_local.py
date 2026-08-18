"""Реальный тест локальных эмбеддингов (fastembed, модель уже скачана).

Отмечено маркером embeds: запускается только при установленном fastembed
(пакет ставится как extra '.[local]'; модель качается в ~/.cache/fastembed
при первом использовании и закэширована на этой машине).
"""

import pytest

from omnes_memory.config import Settings
from omnes_memory.db import Database
from omnes_memory.embedding import LocalFastembed
from omnes_memory.memory_service import MemoryService

MODEL = Settings().embed_model  # paraphrase-multilingual-MiniLM-L12-v2

fastembed = pytest.importorskip("fastembed", reason="не установлен .[local]")


@pytest.fixture(scope="module")
def provider():
    try:
        return LocalFastembed(MODEL)
    except Exception as e:  # модели нет и скачать нельзя (офлайн)
        pytest.skip(f"модель недоступна: {e}")


def test_dim_is_384(provider):
    assert provider.dim == 384


def test_semantic_ranking_russian(provider, tmp_path):
    """Запрос про работу Иванова должен ранжироваться выше борща и погоды."""
    import numpy as np

    docs = [
        "Иванов работает инженером на заводе в Казани",
        "Любимое блюдо — борщ с чесноком",
        "Погода завтра будет солнечной",
    ]
    q = np.asarray(provider.embed_query("где работает инженер Иванов?"))
    vecs = [np.asarray(v) for v in provider.embed(docs)]

    def cos(a, b):
        return float(a @ b / (np.linalg.norm(a) * np.linalg.norm(b)))

    scores = [cos(q, v) for v in vecs]
    assert scores[0] > 0.5          # семантическое совпадение
    assert scores[0] - max(scores[1], scores[2]) > 0.5  # явный отрыв


def test_memory_search_vector_real(provider, tmp_path):
    db = Database(tmp_path / "e.db")
    try:
        svc = MemoryService(db, provider)
        svc.upsert("Иванов работает инженером на заводе в Казани", key="work")
        svc.upsert("Любимое блюдо — борщ с чесноком", key="food")
        hits = svc.search_vector("чем занимается Иванов?", limit=1)
        assert hits and hits[0]["key"] == "work"
    finally:
        db.close()
