"""Экстракция сущностей и отношений (порт OneKE-пайплайна OmnesBOT, фаза 4.2–4.3).

Чанкинг: границы предложений, макс. 3000 симв., перекрытие 300; префильтр
коротких/заголовочных чанков. LLM-извлечение в JSON + пост-обработка:
валидация отношений, инференс по описаниям, фильтр мусорных сущностей.
"""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass, field

from .llm_client import LLMError, LLMProtocol

log = logging.getLogger("omnes.extractor")

CHUNK_MAX_CHARS = 3000
CHUNK_OVERLAP = 300
MIN_CHUNK_CHARS = 80

_SENT_SPLIT_RE = re.compile(r"(?<=[.!?…])\s+|\n+")
_HAS_SENTINEL_RE = re.compile(r"[.!?…]")

EXTRACTION_SYSTEM_PROMPT = """\
Ты — экстрактор сущностей и отношений для графа знаний. Извлеки из текста \
сущности и отношения между ними. Верни СТРОГО JSON без markdown-обёрток:
{"entities": [{"id": "e1", "label": "Имя", "type": "Person", "description": "кратко"}],
 "relations": [{"source": "e1", "target": "e2", "label": "works_at", "contexts": ["фраза"]}]}
Типы: Person | Organization | Location | Event | Concept | Artifact | Other.
Правила: label — точное имя собственное или термин; отношение связывает только \
извлечённые сущности; label отношения — английский snake_case (works_at, father_of, \
located_in, part_of, created, manages, uses, related_to...); contexts — фразы-доказательства.\
Нет сущностей — пустые списки. Только JSON."""

VALID_TYPES = {"Person", "Organization", "Location", "Event", "Concept",
               "Artifact", "Other"}

# Порт _filter_junk_entities: стоп-слова, которые LLM путает с сущностями
JUNK_LABELS = {
    "который", "которая", "которое", "которые", "этот", "эта", "это", "эти",
    "также", "где", "что", "как", "или", "для", "при", "все", "весь", "вся",
    "однако", "кроме", "только", "очень", "когда", "тогда", "такой", "такая",
    "it", "the", "this", "that", "and", "for",
}

# Порт _infer_relations_from_descriptions (базовые ~40 русских шаблонов)
INFERENCE_PATTERNS: list[tuple[str, str]] = [
    ("отец", "father_of"), ("мать", "mother_of"), ("сын", "son_of"),
    ("дочь", "daughter_of"), ("брат", "brother_of"), ("сестра", "sister_of"),
    ("муж", "husband_of"), ("жена", "wife_of"),
    ("работает в", "works_at"), ("работает на", "works_at"),
    ("работал в", "worked_at"), ("сотрудник", "employee_of"),
    ("руководит", "manages"), ("директор", "directs"), ("возглавляет", "heads"),
    ("основал", "founded"), ("основала", "founded"), ("основатель", "founded"),
    ("входит в", "part_of"), ("является частью", "part_of"),
    ("расположен в", "located_in"), ("находится в", "located_in"),
    ("столица", "capital_of"), ("город", "city_of"),
    ("создал", "created"), ("разработал", "developed"), ("написал", "wrote"),
    ("использует", "uses"), ("применяет", "uses"),
    ("партнёр", "partner_of"), ("сотрудничает", "collaborates_with"),
    ("конкурент", "competitor_of"), ("владеет", "owns"),
    ("произошёл в", "occurred_in"), ("произошла в", "occurred_in"),
    ("состоялся в", "held_in"), ("начался в", "started_in"),
    ("связан с", "related_to"), ("относится к", "related_to"),
    ("включает", "includes"), ("содержит", "contains"),
    ("производит", "produces"), ("выпускает", "produces"),
    ("установлен в", "installed_in"), ("выполняет", "performs"),
]


@dataclass
class Entity:
    label: str
    type: str = "Other"
    description: str = ""


@dataclass
class Relation:
    source: str  # label сущности-источника
    target: str  # label сущности-цели
    label: str
    contexts: list[str] = field(default_factory=list)


@dataclass
class ExtractionResult:
    entities: list[Entity] = field(default_factory=list)
    relations: list[Relation] = field(default_factory=list)
    chunks_processed: int = 0
    chunks_skipped: int = 0


# ── Чанкинг ─────────────────────────────────────────────────────────────


def split_sentences(text: str) -> list[str]:
    return [s.strip() for s in _SENT_SPLIT_RE.split(text) if s.strip()]


def split_into_chunks(
    text: str,
    max_chars: int = CHUNK_MAX_CHARS,
    overlap: int = CHUNK_OVERLAP,
) -> list[str]:
    """Порт _split_into_chunks: по границам предложений с перекрытием."""
    sentences = split_sentences(text)
    if not sentences:
        return []
    # сверхдлинные предложения режем жёстко
    prepared: list[str] = []
    for s in sentences:
        while len(s) > max_chars:
            prepared.append(s[:max_chars])
            s = s[max_chars:]
        if s:
            prepared.append(s)

    chunks: list[str] = []
    current = ""
    for s in prepared:
        candidate = (current + " " + s).strip() if current else s
        if len(candidate) > max_chars and current:
            chunks.append(current)
            tail = current[-overlap:]
            current = (tail + " " + s).strip() if overlap else s
        else:
            current = candidate
    if current.strip():
        chunks.append(current)
    return chunks


def prefilter_chunk(chunk: str) -> bool:
    """True — чанк подходит. Порт префильтра: короткие и заголовочные — мимо."""
    stripped = chunk.strip()
    if len(stripped) < MIN_CHUNK_CHARS:
        return False
    # заголовочные оглавления: нет терминаторов предложений и текст короткий
    return bool(_HAS_SENTINEL_RE.search(stripped) or len(stripped) >= 300)


# ── Извлечение ──────────────────────────────────────────────────────────


class Extractor:
    def __init__(self, llm: LLMProtocol, max_chunks: int = 200):
        self.llm = llm
        self.max_chunks = max_chunks

    def extract(
        self, text: str, on_batch=None, batch_size: int = 20,
    ) -> ExtractionResult:
        """Полный пайплайн: чанки → LLM → пост-обработка. on_batch вызывается
        каждые batch_size чанков с промежуточным ExtractionResult (инкремент)."""
        result = ExtractionResult()
        chunks = split_into_chunks(text)
        for i, chunk in enumerate(chunks):
            if i >= self.max_chunks:
                log.warning("Достигнут лимит чанков (%d), остаток пропущен",
                            self.max_chunks)
                break
            if not prefilter_chunk(chunk):
                result.chunks_skipped += 1
                continue
            self._extract_chunk(chunk, result)
            result.chunks_processed += 1
            if on_batch and result.chunks_processed % batch_size == 0:
                on_batch(result)
        merged = self.postprocess(result)
        return merged

    def _extract_chunk(self, chunk: str, result: ExtractionResult) -> None:
        for attempt in range(3):
            try:
                data = self.llm.ask_json(EXTRACTION_SYSTEM_PROMPT, chunk)
                break
            except LLMError as e:
                if attempt == 2:
                    log.warning("Чанк пропущен после 3 попыток: %s", e)
                    return
        if not isinstance(data, dict):
            return
        by_id: dict[str, Entity] = {}
        for raw in data.get("entities", []) or []:
            if not isinstance(raw, dict):
                continue
            label = str(raw.get("label", "")).strip()
            if not label or label.lower() in JUNK_LABELS or len(label) < 3:
                continue
            etype = str(raw.get("type", "Other"))
            if etype not in VALID_TYPES:
                etype = "Other"
            ent = Entity(label=label, type=etype,
                         description=str(raw.get("description", "")).strip()[:500])
            by_id[str(raw.get("id", label))] = ent
            result.entities.append(ent)
        for raw in data.get("relations", []) or []:
            if not isinstance(raw, dict):
                continue
            src, tgt = by_id.get(str(raw.get("source"))), by_id.get(str(raw.get("target")))
            if src is None or tgt is None or src is tgt:
                continue
            label = str(raw.get("label", "")).strip().lower().replace(" ", "_")
            if not label or len(label) > 64:
                continue
            ctx = raw.get("contexts")
            result.relations.append(Relation(
                source=src.label, target=tgt.label, label=label,
                contexts=[str(c)[:300] for c in ctx] if isinstance(ctx, list) else [],
            ))

    # ── Пост-обработка (порт extraction.py) ──

    def postprocess(self, result: ExtractionResult) -> ExtractionResult:
        self._filter_junk(result)
        self._infer_relations(result)
        self._dedup(result)
        return result

    @staticmethod
    def _filter_junk(result: ExtractionResult) -> None:
        result.entities = [
            e for e in result.entities
            if e.label.lower() not in JUNK_LABELS and len(e.label) >= 3
        ]
        valid_labels = {e.label for e in result.entities}
        result.relations = [
            r for r in result.relations
            if r.source in valid_labels and r.target in valid_labels
        ]

    @staticmethod
    def _infer_relations(result: ExtractionResult) -> None:
        """Описание A упоминает label B + шаблонное слово → отношение A→B."""
        existing = {(r.source, r.target, r.label) for r in result.relations}
        labels = [e.label for e in result.entities]
        for entity in result.entities:
            desc = entity.description.lower()
            if not desc:
                continue
            for other in labels:
                if other == entity.label or other.lower() not in desc:
                    continue
                for pattern, relation in INFERENCE_PATTERNS:
                    if pattern in desc:
                        key = (entity.label, other, relation)
                        if key not in existing:
                            existing.add(key)
                            result.relations.append(Relation(
                                source=entity.label, target=other,
                                label=relation,
                                contexts=[f"инференс: {entity.description[:200]}"],
                            ))
                        break

    @staticmethod
    def _dedup(result: ExtractionResult) -> None:
        """Слияние дублей сущностей по (label, type) и склейка описаний."""
        merged: dict[tuple[str, str], Entity] = {}
        for e in result.entities:
            key = (e.label, e.type)
            if key in merged:
                base = merged[key]
                if e.description and e.description not in base.description:
                    base.description = (base.description + " " + e.description).strip()[:1000]
            else:
                merged[key] = e
        result.entities = list(merged.values())
        seen: set[tuple[str, str, str]] = set()
        relations: list[Relation] = []
        for r in result.relations:
            key = (r.source, r.target, r.label)
            if key not in seen:
                seen.add(key)
                relations.append(r)
        result.relations = relations
