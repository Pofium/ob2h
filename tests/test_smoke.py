"""Смоук-тест каркаса: пакет импортируется, окружение соответствует ADR."""

import importlib.metadata
import sqlite3
import sys

import omnes_memory


def test_package_imports_with_version():
    assert omnes_memory.__version__.startswith("0.")


def test_python_at_least_312():
    assert sys.version_info >= (3, 12)


def test_fts5_trigram_available():
    """ADR-3: гибридный поиск строится на FTS5 trigram — проверяем поддержку."""
    con = sqlite3.connect(":memory:")
    con.execute("CREATE VIRTUAL TABLE t USING fts5(x, tokenize='trigram')")
    con.execute("INSERT INTO t VALUES ('проверка русского текста')")
    hits = con.execute("SELECT count(*) FROM t WHERE t MATCH 'русского'").fetchone()
    assert hits[0] == 1


def test_metadata_present():
    assert importlib.metadata.version("omnes-memory")
