"""Тесты дрима: 2 фазы, агентный цикл правок, курсор, git-коммит (фаза 5.1)."""

import json

import pytest

from fakes import FakeLLM
from omnes_memory.config import Settings
from omnes_memory.db import Database
from omnes_memory.dream import Dream
from omnes_memory.gitstore import GitStore
from omnes_memory.workspace import Workspace


@pytest.fixture
def env(tmp_path):
    ws = Workspace(tmp_path / "ws")
    gs = GitStore(ws.root)
    settings = Settings(_env_file=None, data_dir=tmp_path / "data")
    db = Database(settings.db_path)
    yield ws, gs, settings, db
    db.close()


def prepare_history(ws, n=5):
    for i in range(n):
        ws.append_history(f"итог сессии {i}: пользователь работал над проектом Альфа")


def test_dream_full_cycle_applies_edits(env):
    ws, gs, settings, db = env
    prepare_history(ws)
    ws.write("memory", "# Долгосрочная память\n\n- (пусто — дрим и агент наполнят)\n")
    gs.auto_commit("init")

    llm = FakeLLM(responses=[
        "анализ: стоит добавить факт о проекте Альфа",           # фаза 1
        json.dumps({"action": "edit", "file": "memory",
                    "old": "- (пусто — дрим и агент наполнят)",
                    "new": "- Пользователь ведёт проект Альфа"}, ensure_ascii=False),
        json.dumps({"action": "done", "summary": "добавлен факт"}, ensure_ascii=False),
    ])
    dream = Dream(ws, gs, llm, settings, db)
    result = dream.run(trigger="manual")

    assert result["status"] == "ok"
    assert result["processed"] == 5
    assert result["edits"] == 1
    assert result["commit"]
    assert "проект Альфа" in ws.read("memory")
    assert ws.get_cursor("dream_cursor") == 5  # курсор продвинут

    # git-история содержит dream-коммит
    entries = gs.log()
    assert any(e["message"].startswith("dream:") for e in entries)

    # dream_runs залогирован
    row = db.query_one("SELECT status, trigger FROM dream_runs ORDER BY id DESC")
    assert row["status"] == "ok" and row["trigger"] == "manual"


def test_dream_no_new_records(env):
    ws, gs, settings, db = env
    dream = Dream(ws, gs, FakeLLM(), settings, db)
    result = dream.run()
    assert result["status"] == "ok"
    assert result["processed"] == 0
    assert result.get("note")


def test_dream_requires_llm(env):
    ws, gs, settings, db = env
    prepare_history(ws)
    dream = Dream(ws, gs, None, settings, db)
    result = dream.run()
    assert result["status"] == "error"
    row = db.query_one("SELECT status FROM dream_runs ORDER BY id DESC")
    assert row["status"] == "error"


def test_dream_second_run_processes_only_new(env):
    ws, gs, settings, db = env
    prepare_history(ws, 3)
    done = json.dumps({"action": "done", "summary": "ok"})
    llm = FakeLLM(responses=["анализ", done, "анализ2", done])
    dream = Dream(ws, gs, llm, settings, db)
    dream.run()
    prepare_history(ws, 2)  # записи 4, 5
    result = dream.run()
    assert result["processed"] == 2


def test_dream_edit_retry_on_wrong_fragment(env):
    """LLM промахнулся по фрагменту — получает ошибку и исправляется."""
    ws, gs, settings, db = env
    prepare_history(ws, 2)
    llm = FakeLLM(responses=[
        "анализ",
        json.dumps({"action": "edit", "file": "memory",
                    "old": "такого фрагмента нет", "new": "x"}),      # промах
        json.dumps({"action": "read", "file": "memory"}),             # перечитал
        json.dumps({"action": "edit", "file": "memory",
                    "old": "# Долгосрочная память", "new": "# Память агента"},
                   ensure_ascii=False),
        json.dumps({"action": "done", "summary": "ok"}),
    ])
    dream = Dream(ws, gs, llm, settings, db)
    result = dream.run()
    assert result["edits"] == 1
    assert ws.read("memory").startswith("# Память агента")


def test_dream_batch_limit(env):
    ws, gs, settings, db = env
    prepare_history(ws, 50)
    llm = FakeLLM(responses=["анализ", json.dumps({"action": "done", "summary": "s"})])
    settings_small = Settings(_env_file=None, data_dir=settings.data_dir,
                              dream_batch=20)
    result = Dream(ws, gs, llm, settings_small, db).run()
    assert result["processed"] == 20  # батч 20 (порт из OmnesBOT)


def test_dream_restore_via_git(env):
    ws, gs, settings, db = env
    prepare_history(ws)
    gs.auto_commit("init")  # исходное состояние до дрима
    llm = FakeLLM(responses=[
        "анализ",
        json.dumps({"action": "edit", "file": "user", "old": "# USER",
                    "new": "# USER\n- Владелец любит кофе"}, ensure_ascii=False),
        json.dumps({"action": "done", "summary": "ok"}),
    ])
    Dream(ws, gs, llm, settings, db).run()
    assert "кофе" in ws.read("user")
    first = gs.log()[-1]["sha"]  # самый старый коммит (до дрима)
    gs.restore(first)
    assert "кофе" not in ws.read("user")


# --- dream-extract: сессии попадают в общий граф (дополнение фазы 5) ---


def test_dream_extracts_sessions_into_graph(env):
    from omnes_memory.embedding import FakeEmbedding
    from omnes_memory.graph_service import GraphService

    ws, gs, settings, db = env
    for i in range(3):
        ws.append_history(
            f"сессия {i}: обсуждали проект Альфа, заказчик ООО Ромашка, "
            f"дедлайн в сентябре. Иванов назначен ответственным."
        )
    graph = GraphService(db, FakeEmbedding())
    extraction = json.dumps({
        "entities": [
            {"id": "e1", "label": "Проект Альфа", "type": "Concept",
             "description": "проект с дедлайном в сентябре"},
            {"id": "e2", "label": "ООО Ромашка", "type": "Organization",
             "description": "заказчик проекта Альфа"},
            {"id": "e3", "label": "Иванов", "type": "Person",
             "description": "ответственный за проект Альфа"},
        ],
        "relations": [
            {"source": "e1", "target": "e2", "label": "customer_is",
             "contexts": ["заказчик"]},
        ],
    }, ensure_ascii=False)
    llm = FakeLLM(responses=["анализ", json.dumps({"action": "done",
                                                   "summary": "ok"}), extraction])
    result = Dream(ws, gs, llm, settings, db, graph=graph).run()
    assert result["status"] == "ok"
    assert result["graph_entities"] == 3
    assert result["graph_edges"] == 1
    # сессии и документы — один граф: узлы доступны обычным поиском
    found = graph.search("Иванов")
    assert any(n["label"] == "Иванов" for n in found["nodes"])


def test_dream_extract_merges_with_document_nodes(env):
    """Узел из сессии склеивается с узлом из документа (дедуп по label|type)."""
    from omnes_memory.embedding import FakeEmbedding
    from omnes_memory.extractor import Entity, ExtractionResult
    from omnes_memory.graph_service import GraphService

    ws, gs, settings, db = env
    ws.append_history(
        "сессия: Иванов снова подтвердил сроки по проекту Альфа и сообщил, "
        "что заказчик доволен промежуточными результатами работы."
    )
    graph = GraphService(db, FakeEmbedding())
    graph.upsert_extraction(ExtractionResult(
        entities=[Entity("Иванов", "Person", "инженер из документа")],
        relations=[],
    ))
    extraction = json.dumps({
        "entities": [{"id": "e1", "label": "Иванов", "type": "Person",
                      "description": "подтвердил сроки"}],
        "relations": [],
    }, ensure_ascii=False)
    llm = FakeLLM(responses=["анализ", json.dumps({"action": "done",
                                                   "summary": "ok"}), extraction])
    Dream(ws, gs, llm, settings, db, graph=graph).run()
    row = db.query_one("SELECT count(*) AS c FROM graph_nodes WHERE label='Иванов'")
    assert row["c"] == 1  # один узел, не два
    val = db.query_one("SELECT val FROM graph_nodes WHERE label='Иванов'")["val"]
    assert val == 2  # упоминания суммируются


def test_dream_extract_disabled_by_config(env):
    from omnes_memory.config import Settings as Cfg
    from omnes_memory.embedding import FakeEmbedding
    from omnes_memory.graph_service import GraphService

    ws, gs, settings, db = env
    ws.append_history("сессия с сущностями, но экстракция выключена.")
    off = Cfg(_env_file=None, data_dir=settings.data_dir, dream_extract_enabled=False)
    graph = GraphService(db, FakeEmbedding())
    llm = FakeLLM(responses=["анализ", json.dumps({"action": "done",
                                                   "summary": "ok"})])
    result = Dream(ws, gs, llm, off, db, graph=graph).run()
    assert result["status"] == "ok"
    assert "graph_entities" not in result
    assert graph.stats()["nodes"] == 0


def test_dream_extract_failure_does_not_break_dream(env):
    from omnes_memory.embedding import FakeEmbedding
    from omnes_memory.graph_service import GraphService

    ws, gs, settings, db = env
    ws.append_history("сессия с длинным содержимым, чтобы прошёл префильтр. " * 5)

    class BrokenExtractLLM(FakeLLM):
        def ask_json(self, *a, **k):
            if len(self.calls) >= 2:  # третий вызов — экстракция
                from omnes_memory.llm_client import LLMError
                raise LLMError("extract boom")
            return super().ask_json(*a, **k)

    llm = BrokenExtractLLM(
        responses=["анализ", json.dumps({"action": "done", "summary": "ok"})]
    )
    graph = GraphService(db, FakeEmbedding())
    result = Dream(ws, gs, llm, settings, db, graph=graph).run()
    assert result["status"] == "ok"          # дрим не упал
    # экстрактор после 3 неудачных попыток пропускает чанк внутри себя:
    # дрим продолжает работать, граф пуст, без graph_error
    assert result["graph_entities"] == 0
    assert graph.stats()["nodes"] == 0
