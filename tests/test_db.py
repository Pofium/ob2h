"""Тесты ядра хранения: миграции, FTS5-trigram, триггеры (фаза 1.2, 1.5)."""

import sqlite3

import pytest

from omnes_memory.db import SCHEMA_VERSION, Database


@pytest.fixture
def db(tmp_path):
    database = Database(tmp_path / "test.db")
    yield database
    database.close()


def test_migrations_create_tables(db):
    tables = {
        r["name"]
        for r in db.query("SELECT name FROM sqlite_master WHERE type='table'")
    }
    for expected in (
        "kv", "memories", "memory_relations", "documents", "chunks",
        "graph_nodes", "graph_edges", "dream_runs",
    ):
        assert expected in tables
    assert db.kv_get("schema_version") == str(SCHEMA_VERSION)


def test_migrations_idempotent(tmp_path):
    path = tmp_path / "test.db"
    d1 = Database(path)
    d1.close()
    d2 = Database(path)  # повторное открытие не падает и не дублирует
    assert d2.kv_get("schema_version") == str(SCHEMA_VERSION)
    d2.close()


def test_fts5_russian_roundtrip(db):
    db.execute(
        "INSERT INTO memories (key, content, created_at, updated_at) "
        "VALUES ('k1', 'Пользователь любит кофе по утрам', '2026-01-01', '2026-01-01')"
    )
    hits = db.query(
        "SELECT rowid FROM memories_fts WHERE memories_fts MATCH ? ",
        (Database.fts_query("кофе"),),
    )
    assert len(hits) == 1
    assert hits[0]["rowid"] == 1


def test_fts5_triggers_update_and_delete(db):
    def ins(key, content):
        db.execute(
            "INSERT INTO memories (key, content, created_at, updated_at) "
            "VALUES (?, ?, '2026', '2026')", (key, content),
        )

    def fts_count(match):
        return len(db.query(
            "SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?",
            (Database.fts_query(match),),
        ))

    ins("k1", "котёл паровой работает")
    assert fts_count("паровой") == 1

    db.execute("UPDATE memories SET content='дизельный агрегат' WHERE key='k1'")
    assert fts_count("паровой") == 0
    assert fts_count("дизельный") == 1

    db.execute("DELETE FROM memories WHERE key='k1'")
    assert fts_count("дизельный") == 0


def test_fts_query_escapes_and_short_queries():
    assert Database.fts_query('кофе "в зернах"') == '"кофе в зернах"'
    assert Database.fts_query("ab") == '""'  # короче trigram — пустой результат


def test_chunks_fts(db):
    db.execute(
        "INSERT INTO documents (title, created_at) VALUES ('doc', '2026')"
    )
    db.execute(
        "INSERT INTO chunks (doc_id, ordinal, text, created_at) "
        "VALUES (1, 0, 'техническое задание на систему', '2026')"
    )
    hits = db.query(
        "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?",
        (Database.fts_query("задание"),),
    )
    assert len(hits) == 1


def test_graph_unique_constraints(db):
    db.execute(
        "INSERT INTO graph_nodes (node_id, label, node_type, created_at, updated_at) "
        "VALUES ('n1', 'Пётр', 'Person', '2026', '2026')"
    )
    with pytest.raises(sqlite3.IntegrityError):
        db.execute(
            "INSERT INTO graph_nodes (node_id, label, node_type, created_at, updated_at) "
            "VALUES ('n1', 'Пётр', 'Person', '2026', '2026')"
        )


def test_kv_json_roundtrip(db):
    db.kv_set_json("state", {"a": 1, "b": ["x"]})
    assert db.kv_get_json("state") == {"a": 1, "b": ["x"]}
    assert db.kv_get_json("missing", default=[]) == []
