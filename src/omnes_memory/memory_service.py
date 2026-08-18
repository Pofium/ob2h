"""Сервис памяти: гибридный поиск FTS5+вектор, RRF k=60 (порт MemoryService из OmnesBOT).

Формулы перенесены без изменений:
- RRF: score = Σ 1/(60 + rank + 1), rank с нуля;
- build_context: top-30 по важности, combined = 0.6*importance + 0.4*word-overlap.
"""

from __future__ import annotations

import json
import re
import uuid
from typing import Any

from .db import Database, utcnow
from .embedding import EmbeddingProvider
from .vector import serialize, top_k

RRF_K = 60
_WORD_RE = re.compile(r"[а-яёa-z0-9]+", re.IGNORECASE)


class MemoryService:
    def __init__(self, db: Database, embedder: EmbeddingProvider):
        self.db = db
        self.embedder = embedder
        self._ensure_embed_dim()

    def _ensure_embed_dim(self) -> None:
        stored = self.db.kv_get("embed_dim")
        if stored is None:
            self.db.kv_set("embed_dim", str(self.embedder.dim))
        elif int(stored) != self.embedder.dim:
            raise RuntimeError(
                f"Размерность эмбеддингов изменилась ({stored} -> {self.embedder.dim}). "
                "Задайте прежний провайдер/модель или пересоберите индекс "
                "(удалите data/omnes.db)."
            )

    # --- запись ---

    def upsert(
        self,
        content: str,
        key: str | None = None,
        category: str = "general",
        importance: float = 0.5,
        source: str = "manual",
        meta: dict[str, Any] | None = None,
        reembed: bool = True,
    ) -> dict[str, Any]:
        key = key or uuid.uuid4().hex[:12]
        now = utcnow()
        blob = serialize(self.embedder.embed([content])[0]) if reembed else None
        existing = self.db.query_one("SELECT id FROM memories WHERE key=?", (key,))
        if existing:
            self.db.execute(
                "UPDATE memories SET content=?, category=?, importance=?, source=?, "
                "meta=?, updated_at=? WHERE key=?",
                (content, category, importance, source,
                 json.dumps(meta, ensure_ascii=False) if meta else None, now, key),
            )
            if blob is not None:
                self.db.execute("UPDATE memories SET embedding=? WHERE key=?", (blob, key))
            return {"key": key, "status": "updated"}
        self.db.execute(
            "INSERT INTO memories (key, content, category, importance, source, meta, "
            "embedding, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
            (key, content, category, importance, source,
             json.dumps(meta, ensure_ascii=False) if meta else None, blob, now, now),
        )
        return {"key": key, "status": "created"}

    # --- поиск ---

    def search_fts(self, query: str, limit: int = 10) -> list[dict[str, Any]]:
        match = Database.fts_query(query)
        if match == '""':
            return []
        rows = self.db.query(
            "SELECT m.key, m.content, m.category, m.importance, "
            "bm25(memories_fts) AS fts_rank "
            "FROM memories_fts f JOIN memories m ON m.id = f.rowid "
            "WHERE memories_fts MATCH ? ORDER BY fts_rank LIMIT ?",
            (match, limit),
        )
        return [dict(r) for r in rows]

    def search_vector(
        self, query: str, limit: int = 10, category: str | None = None
    ) -> list[dict[str, Any]]:
        qvec = self.embedder.embed_query(query)
        rows = self.db.query(
            "SELECT id, key, embedding FROM memories"
            + (" WHERE category=?" if category else "")
            + " ORDER BY id",
            ((category,) if category else ()),
        )
        scored = top_k(qvec, [(r["id"], r["embedding"]) for r in rows], k=limit)
        result = []
        for mid, score in scored:
            row = self.db.query_one(
                "SELECT key, content, category, importance FROM memories WHERE id=?",
                (mid,),
            )
            if row:
                d = dict(row)
                d["vector_score"] = round(float(score), 4)
                result.append(d)
        return result

    def search_hybrid(
        self, query: str, limit: int = 10, category: str | None = None
    ) -> list[dict[str, Any]]:
        """RRF-слияние FTS и векторного результатов (k=60, как в OmnesBOT)."""
        pool = limit * 2
        fts = self.search_fts(query, limit=pool)
        vec = self.search_vector(query, limit=pool, category=category)
        if category:
            fts = [r for r in fts if r["category"] == category]

        rrf: dict[str, float] = {}
        for rank, r in enumerate(fts):
            rrf[r["key"]] = rrf.get(r["key"], 0.0) + 1.0 / (RRF_K + rank + 1)
        for rank, r in enumerate(vec):
            rrf[r["key"]] = rrf.get(r["key"], 0.0) + 1.0 / (RRF_K + rank + 1)

        all_keys = {r["key"] for r in fts} | {r["key"] for r in vec}
        by_key = {r["key"]: r for r in fts + vec}
        scored = sorted(all_keys, key=lambda k: rrf.get(k, 0.0), reverse=True)[:limit]

        result = []
        for key in scored:
            d = by_key[key]
            d["rrf_score"] = round(rrf[key], 6)
            result.append(d)
        return result

    # --- чтение / изменение ---

    def get(self, key: str) -> dict[str, Any] | None:
        row = self.db.query_one("SELECT * FROM memories WHERE key=?", (key,))
        if not row:
            return None
        self.db.execute(
            "UPDATE memories SET access_count=access_count+1, last_accessed=? "
            "WHERE key=?",
            (utcnow(), key),
        )
        d = dict(row)
        if d.get("meta"):
            d["meta"] = json.loads(d["meta"])
        d.pop("embedding", None)
        return d

    def update(
        self, key: str, content: str | None = None, importance: float | None = None,
        category: str | None = None,
    ) -> str:
        if not self.db.query_one("SELECT 1 FROM memories WHERE key=?", (key,)):
            return "not_found"
        if content is not None:
            blob = serialize(self.embedder.embed([content])[0])
            self.db.execute(
                "UPDATE memories SET content=?, embedding=?, updated_at=? WHERE key=?",
                (content, blob, utcnow(), key),
            )
        if importance is not None:
            self.db.execute(
                "UPDATE memories SET importance=?, updated_at=? WHERE key=?",
                (importance, utcnow(), key),
            )
        if category is not None:
            self.db.execute(
                "UPDATE memories SET category=?, updated_at=? WHERE key=?",
                (category, utcnow(), key),
            )
        return "updated"

    def forget(self, key: str) -> str:
        cur = self.db.execute("DELETE FROM memories WHERE key=?", (key,))
        return "deleted" if cur.rowcount else "not_found"

    # --- важность (порт механики OmnesBOT) ---

    def decay_importance(self, rate: float = 0.01, min_importance: float = 0.05) -> int:
        """Затухание важности редко используемых воспоминаний."""
        cur = self.db.execute(
            "UPDATE memories SET importance=MAX(?, importance*(1-?)) "
            "WHERE access_count < 2 AND importance > ?",
            (min_importance, rate, min_importance),
        )
        return cur.rowcount

    def purge_weak(self, threshold: float = 0.05, max_access: int = 2) -> int:
        cur = self.db.execute(
            "DELETE FROM memories WHERE importance < ? AND access_count < ?",
            (threshold, max_access),
        )
        return cur.rowcount

    def top_by_importance(self, limit: int = 30) -> list[dict[str, Any]]:
        rows = self.db.query(
            "SELECT key, content, category, importance FROM memories "
            "ORDER BY importance DESC, updated_at DESC LIMIT ?",
            (limit,),
        )
        return [dict(r) for r in rows]

    # --- контекст для промпта ---

    def build_context(self, query: str = "", max_tokens: int = 1000) -> str:
        """Блок <agent_memory>: топ по важности, скоринг 0.6*imp + 0.4*relevance."""
        max_chars = max_tokens * 4
        memories = self.top_by_importance(limit=30)
        query_words = set(_WORD_RE.findall(query.lower()))
        scored = []
        for m in memories:
            content_words = set(_WORD_RE.findall(m["content"].lower()))
            overlap = len(query_words & content_words)
            relevance = overlap / max(len(query_words), 1) if query_words else 0.0
            combined = 0.6 * m["importance"] + 0.4 * relevance
            scored.append((combined, m))
        scored.sort(key=lambda x: x[0], reverse=True)

        lines = ["<agent_memory>"]
        used = len(lines[0])
        for _, m in scored:
            entry = f"- [{m['category']}] {m['content']}"
            if used + len(entry) > max_chars:
                break
            lines.append(entry)
            used += len(entry)
        lines.append("</agent_memory>")
        return "\n".join(lines)

    def stats(self) -> dict[str, Any]:
        def count(table: str) -> int:
            return self.db.query_one(f"SELECT count(*) AS c FROM {table}")["c"]  # noqa: S608

        return {
            "memories": count("memories"),
            "relations": count("memory_relations"),
        }
