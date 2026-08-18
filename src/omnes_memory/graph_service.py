"""Граф знаний: upsert с дедупом, гибридный поиск, KAG-рассуждение.

Порт PG-пути KAGReasoningService из OmnesBOT (без Neo4j, ADR-6):
- скоринг label=10 / name=5 / desc=1;
- расширение на 1-hop соседей;
- reason(): блок фактов → один LLM-вызов → JSON с confidence.
"""

from __future__ import annotations

import hashlib
import json
import logging
from typing import Any

from .db import Database, utcnow
from .embedding import EmbeddingProvider
from .extractor import ExtractionResult
from .llm_client import LLMError, LLMProtocol
from .vector import serialize, top_k

log = logging.getLogger("omnes.graph")

REASON_SYSTEM_PROMPT = """\
Ты отвечаешь на вопрос по графу знаний личного агента. Опирайся ТОЛЬКО на \
переданные факты. Верни СТРОГО JSON без markdown:
{"answer": "ответ по-русски",
 "confidence": 0.0,
 "reasoning_steps": ["шаг 1", "шаг 2"],
 "used_entities": ["label сущностей"],
 "used_relations": ["label отношений"]}
Если фактов недостаточно — так и скажи в answer, confidence = 0.1."""


def make_node_id(label: str, node_type: str) -> str:
    return hashlib.sha256(f"{label}|{node_type}".encode()).hexdigest()[:24]


class GraphService:
    def __init__(self, db: Database, embedder: EmbeddingProvider):
        self.db = db
        self.embedder = embedder

    # --- запись ---

    def upsert_extraction(self, result: ExtractionResult) -> dict[str, int]:
        """Дедуп по node_id: val++ и склейка описаний; рёбра — weight++."""
        now = utcnow()
        label_to_rowid: dict[str, int] = {}

        new_entities = updated_entities = new_edges = 0
        for entity in result.entities:
            node_id = make_node_id(entity.label, entity.type)
            existing = self.db.query_one(
                "SELECT id, description FROM graph_nodes WHERE node_id=?", (node_id,)
            )
            if existing:
                desc = existing["description"] or ""
                if entity.description and entity.description not in desc:
                    desc = (desc + " " + entity.description).strip()[:2000]
                self.db.execute(
                    "UPDATE graph_nodes SET val=val+1, description=?, updated_at=? "
                    "WHERE id=?",
                    (desc or None, now, existing["id"]),
                )
                updated_entities += 1
                label_to_rowid[entity.label] = existing["id"]
                # описание изменилось — перевекторизуем
                self._embed_node(existing["id"], f"{entity.label}: {desc}")
            else:
                cur = self.db.execute(
                    "INSERT INTO graph_nodes (node_id, label, node_type, description, "
                    "created_at, updated_at) VALUES (?,?,?,?,?,?)",
                    (node_id, entity.label, entity.type,
                     entity.description or None, now, now),
                )
                new_entities += 1
                label_to_rowid[entity.label] = cur.lastrowid
                self._embed_node(cur.lastrowid,
                                 f"{entity.label}: {entity.description}")

        for rel in result.relations:
            src, tgt = label_to_rowid.get(rel.source), label_to_rowid.get(rel.target)
            if not src or not tgt:
                continue
            existing = self.db.query_one(
                "SELECT id, contexts FROM graph_edges "
                "WHERE source_id=? AND target_id=? AND label=?",
                (src, tgt, rel.label),
            )
            if existing:
                contexts = json.loads(existing["contexts"] or "[]")
                contexts.extend(c for c in rel.contexts if c not in contexts)
                self.db.execute(
                    "UPDATE graph_edges SET weight=weight+1, contexts=? WHERE id=?",
                    (json.dumps(contexts[:20], ensure_ascii=False), existing["id"]),
                )
            else:
                self.db.execute(
                    "INSERT INTO graph_edges (source_id, target_id, label, contexts, "
                    "created_at) VALUES (?,?,?,?,?)",
                    (src, tgt, rel.label,
                     json.dumps(rel.contexts[:20], ensure_ascii=False), now),
                )
                new_edges += 1

        return {"new_entities": new_entities, "updated_entities": updated_entities,
                "new_edges": new_edges}

    def _embed_node(self, rowid: int, text: str) -> None:
        if not text.strip():
            return
        try:
            vec = self.embedder.embed([text])[0]
            self.db.execute("UPDATE graph_nodes SET embedding=? WHERE id=?",
                            (serialize(vec), rowid))
        except Exception as e:  # эмбеддинг узла не критичен
            log.warning("эмбеддинг узла %s не удался: %s", rowid, e)

    # --- поиск ---

    def search(self, query: str, limit: int = 10,
               expand_hops: bool = True) -> dict[str, Any]:
        """Гибрид: ILIKE-скоринг (label=10/name=5/desc=1) + вектор; 1-hop соседи."""
        words = [w for w in query.split() if len(w) >= 3]
        scored: dict[int, float] = {}
        for row in self.db.query(
            "SELECT id, label, description FROM graph_nodes"
        ):
            score = 0.0
            label, desc = row["label"].lower(), (row["description"] or "").lower()
            for w in words:
                if w.lower() in label:
                    score += 10.0
                elif w.lower() in desc:
                    score += 1.0
            if score:
                scored[row["id"]] = score

        # векторная ветка
        try:
            qvec = self.embedder.embed_query(query)
            rows = self.db.query("SELECT id, embedding FROM graph_nodes "
                                 "WHERE embedding IS NOT NULL")
            for nid, vscore in top_k(qvec, [(r["id"], r["embedding"]) for r in rows],
                                     k=limit):
                scored[nid] = scored.get(nid, 0.0) + vscore * 5.0
        except Exception as e:
            log.warning("векторная ветка поиска недоступна: %s", e)

        top_ids = sorted(scored, key=lambda i: scored[i], reverse=True)[:limit]
        nodes = {}
        if top_ids:
            marks = ",".join("?" * len(top_ids))
            for row in self.db.query(
                f"SELECT * FROM graph_nodes WHERE id IN ({marks})", top_ids  # noqa: S608
            ):
                nodes[row["id"]] = dict(row)
                nodes[row["id"]].pop("embedding", None)

        edges: list[dict[str, Any]] = []
        if nodes:
            ids = list(nodes)
            marks = ",".join("?" * len(ids))
            for row in self.db.query(
                f"SELECT e.*, s.label AS source_label, t.label AS target_label "
                f"FROM graph_edges e "
                f"JOIN graph_nodes s ON s.id = e.source_id "
                f"JOIN graph_nodes t ON t.id = e.target_id "
                f"WHERE e.source_id IN ({marks}) OR e.target_id IN ({marks})",  # noqa: S608
                ids + ids,
            ):
                edges.append(dict(row))

        # 1-hop расширение: добавить соседей найденных узлов
        if expand_hops and edges:
            for row in edges:
                for key in ("source_id", "target_id"):
                    nid = row[key]
                    if nid not in nodes:
                        n = self.db.query_one(
                            "SELECT * FROM graph_nodes WHERE id=?", (nid,)
                        )
                        if n:
                            d = dict(n)
                            d.pop("embedding", None)
                            nodes[nid] = d

        return {"nodes": list(nodes.values()), "edges": edges}

    # --- рассуждение ---

    def reason(self, query: str, llm: LLMProtocol) -> dict[str, Any]:
        found = self.search(query, limit=15)
        if not found["nodes"]:
            return {"answer": "В графе нет данных по запросу.", "confidence": 0.0,
                    "reasoning_steps": [], "used_entities": [], "used_relations": []}

        facts = ["Сущности:"]
        for n in found["nodes"]:
            desc = f" — {n['description']}" if n["description"] else ""
            facts.append(f"- {n['label']} ({n['node_type']}){desc}")
        facts.append("Отношения:")
        for e in found["edges"][:40]:
            facts.append(f"- {e['source_label']} --[{e['label']}]--> {e['target_label']}")
        facts_block = "\n".join(facts)

        try:
            answer = llm.ask_json(REASON_SYSTEM_PROMPT,
                                  f"Вопрос: {query}\n\n{facts_block}")
        except LLMError as e:
            log.warning("graph_reason LLM не ответил: %s", e)
            return {"answer": f"[Error] LLM недоступен: {e}", "confidence": 0.0,
                    "reasoning_steps": [], "used_entities": [],
                    "used_relations": [], "facts": facts_block}

        if not isinstance(answer, dict):
            answer = {"answer": str(answer), "confidence": 0.3}
        answer.setdefault("confidence", 0.3)
        answer.setdefault("reasoning_steps", [])
        answer.setdefault("used_entities", [])
        answer.setdefault("used_relations", [])
        answer["graph_stats"] = {
            "nodes_used": len(found["nodes"]), "edges_used": len(found["edges"]),
        }
        return answer

    def stats(self) -> dict[str, int]:
        nodes = self.db.query_one("SELECT count(*) AS c FROM graph_nodes")["c"]
        edges = self.db.query_one("SELECT count(*) AS c FROM graph_edges")["c"]
        docs = self.db.query_one("SELECT count(*) AS c FROM documents")["c"]
        chunks = self.db.query_one("SELECT count(*) AS c FROM chunks")["c"]
        return {"nodes": nodes, "edges": edges, "documents": docs, "chunks": chunks}
