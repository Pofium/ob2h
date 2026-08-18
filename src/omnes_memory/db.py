"""SQLite-хранилище: подключение, версионные миграции, FTS5-trigram (ADR-1…ADR-3).

Одно соединение + RLock: SQLite не любит конкурентных писателей, а серверу хватает
одного потока обработки запросов плюс фоновый поток автодрима.
"""

from __future__ import annotations

import json
import sqlite3
import threading
from collections.abc import Iterable
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1

_MIGRATIONS: dict[int, str] = {
    1: """
    CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    CREATE TABLE IF NOT EXISTS memories (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      key TEXT UNIQUE NOT NULL,
      content TEXT NOT NULL,
      category TEXT NOT NULL DEFAULT 'general',
      importance REAL NOT NULL DEFAULT 0.5,
      source TEXT DEFAULT 'manual',
      meta TEXT,
      embedding BLOB,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      access_count INTEGER NOT NULL DEFAULT 0,
      last_accessed TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_memories_cat_imp
      ON memories (category, importance DESC);

    CREATE TABLE IF NOT EXISTS memory_relations (
      source_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
      target_key TEXT NOT NULL REFERENCES memories(key) ON DELETE CASCADE,
      relation_type TEXT NOT NULL,
      weight REAL NOT NULL DEFAULT 1.0,
      UNIQUE (source_key, target_key, relation_type)
    );

    CREATE TABLE IF NOT EXISTS documents (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      title TEXT,
      path TEXT,
      meta TEXT,
      created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS chunks (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
      ordinal INTEGER NOT NULL,
      text TEXT NOT NULL,
      embedding BLOB,
      created_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_chunks_doc ON chunks (doc_id);

    CREATE TABLE IF NOT EXISTS graph_nodes (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      node_id TEXT UNIQUE NOT NULL,
      label TEXT NOT NULL,
      node_type TEXT NOT NULL,
      description TEXT,
      val INTEGER NOT NULL DEFAULT 1,
      embedding BLOB,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_graph_nodes_label ON graph_nodes (label);
    CREATE INDEX IF NOT EXISTS idx_graph_nodes_type ON graph_nodes (node_type);

    CREATE TABLE IF NOT EXISTS graph_edges (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      source_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
      target_id INTEGER NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
      label TEXT NOT NULL,
      weight REAL NOT NULL DEFAULT 1.0,
      contexts TEXT,
      created_at TEXT NOT NULL,
      UNIQUE (source_id, target_id, label)
    );
    CREATE INDEX IF NOT EXISTS idx_graph_edges_src ON graph_edges (source_id);
    CREATE INDEX IF NOT EXISTS idx_graph_edges_dst ON graph_edges (target_id);

    CREATE TABLE IF NOT EXISTS dream_runs (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      started_at TEXT,
      finished_at TEXT,
      status TEXT,
      trigger TEXT,
      phase_log TEXT,
      stats TEXT
    );

    CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
      content, content='memories', content_rowid='id', tokenize='trigram'
    );
    CREATE TRIGGER IF NOT EXISTS memories_fts_ai AFTER INSERT ON memories BEGIN
      INSERT INTO memories_fts (rowid, content) VALUES (new.id, new.content);
    END;
    CREATE TRIGGER IF NOT EXISTS memories_fts_ad AFTER DELETE ON memories BEGIN
      INSERT INTO memories_fts (memories_fts, rowid, content)
        VALUES ('delete', old.id, old.content);
    END;
    CREATE TRIGGER IF NOT EXISTS memories_fts_au AFTER UPDATE ON memories BEGIN
      INSERT INTO memories_fts (memories_fts, rowid, content)
        VALUES ('delete', old.id, old.content);
      INSERT INTO memories_fts (rowid, content) VALUES (new.id, new.content);
    END;

    CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
      text, content='chunks', content_rowid='id', tokenize='trigram'
    );
    CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
      INSERT INTO chunks_fts (rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
      INSERT INTO chunks_fts (chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE ON chunks BEGIN
      INSERT INTO chunks_fts (chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
      INSERT INTO chunks_fts (rowid, text) VALUES (new.id, new.text);
    END;
    """,
}


def utcnow() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds")


class Database:
    """Обёртка над sqlite3: потокобезопасность, миграции, удобные хелперы."""

    def __init__(self, path: str | Path):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        self._conn = sqlite3.connect(
            str(self.path),
            check_same_thread=False,
            timeout=30.0,
        )
        self._conn.row_factory = sqlite3.Row
        with self._lock:
            self._conn.execute("PRAGMA journal_mode=WAL")
            self._conn.execute("PRAGMA foreign_keys=ON")
            self._conn.execute("PRAGMA synchronous=NORMAL")
        self._migrate()

    # --- миграции ---

    def _migrate(self) -> None:
        with self._lock:
            self._conn.execute(
                "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)"
            )
            row = self._conn.execute(
                "SELECT value FROM kv WHERE key='schema_version'"
            ).fetchone()
            current = int(row["value"]) if row else 0
            for version in sorted(_MIGRATIONS):
                if version > current:
                    self._conn.executescript(_MIGRATIONS[version])
                    self._conn.execute(
                        "INSERT INTO kv (key, value) VALUES ('schema_version', ?) "
                        "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        (str(version),),
                    )
                    self._conn.commit()
                    current = version

    # --- выполнение ---

    def execute(self, sql: str, params: Iterable[Any] = ()) -> sqlite3.Cursor:
        with self._lock:
            cur = self._conn.execute(sql, tuple(params))
            self._conn.commit()
            return cur

    def executemany(self, sql: str, seq: Iterable[Iterable[Any]]) -> None:
        with self._lock:
            self._conn.executemany(sql, [tuple(p) for p in seq])
            self._conn.commit()

    def query(self, sql: str, params: Iterable[Any] = ()) -> list[sqlite3.Row]:
        with self._lock:
            return self._conn.execute(sql, tuple(params)).fetchall()

    def query_one(self, sql: str, params: Iterable[Any] = ()) -> sqlite3.Row | None:
        with self._lock:
            return self._conn.execute(sql, tuple(params)).fetchone()

    def transaction(self) -> threading.RLock:
        """Контекст блокировки для составных операций: with db.transaction(): ..."""
        return self._lock

    # --- kv ---

    def kv_get(self, key: str) -> str | None:
        row = self.query_one("SELECT value FROM kv WHERE key=?", (key,))
        return row["value"] if row else None

    def kv_set(self, key: str, value: str) -> None:
        self.execute(
            "INSERT INTO kv (key, value) VALUES (?, ?) "
            "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            (key, value),
        )

    def kv_get_json(self, key: str, default: Any = None) -> Any:
        raw = self.kv_get(key)
        return json.loads(raw) if raw else default

    def kv_set_json(self, key: str, value: Any) -> None:
        self.kv_set(key, json.dumps(value, ensure_ascii=False))

    def close(self) -> None:
        with self._lock:
            self._conn.close()

    # --- FTS ---

    @staticmethod
    def fts_query(text: str) -> str:
        """Экранирование пользовательского запроса в FTS5-фразу (trigram >= 3 симв.)."""
        cleaned = " ".join(text.replace('"', " ").split())
        if len(cleaned) < 3:
            # trigram не ищет короче 3 символов — расширяем пробелами бессмысленно,
            # отдаём заведомо пустой результат через несуществующую фразу
            cleaned = ""
        return f'"{cleaned}"' if cleaned else '""'
