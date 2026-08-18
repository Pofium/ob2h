"""Дриминг: фоновая консолидация памяти «во сне» (порт Dream из OmnesBOT).

Фаза 1 — LLM-анализ новой истории на фоне текущих MEMORY/SOUL/USER.
Фаза 2 — агентный цикл (≤10 итераций): LLM точечно правит MD-файлы
действиями read/edit/done (аналог ReadFile/EditFile из AgentRunner).
Далее: продвижение .dream_cursor, compact_history, git auto-commit, журнал dream_runs.
"""

from __future__ import annotations

import json
import logging
from datetime import datetime
from typing import Any

from .config import Settings
from .db import Database, utcnow
from .gitstore import GitStore
from .llm_client import LLMError, LLMProtocol
from .workspace import Workspace

log = logging.getLogger("omnes.dream")

MAX_ITERATIONS = 10

PHASE1_SYSTEM = """\
Ты — аналитик памяти личного агента Hermes. Проанализируй новые записи \
истории диалогов на фоне текущего состояния памяти. Найди: устойчивые факты \
о пользователе и его проектах; изменения, противоречащие памяти; что стоит \
добавить в MEMORY.md (факты) или USER.md (о владельце); что устарело и его \
пора поправить. Отвечай кратко по-русски списком. Это анализ — файлы правит \
следующая фаза."""

PHASE2_SYSTEM = """\
Ты — редактор памяти личного агента. Твоя задача — внести точечные правки в \
MD-файлы памяти по анализу. За один шаг — РОВНО ОДНО действие, верни СТРОГО JSON:
{"action": "edit", "file": "memory|soul|user", "old": "точный существующий фрагмент",
 "new": "замена"}
или {"action": "read", "file": "memory|soul|user"} — перечитать файл,
или {"action": "done", "summary": "что изменено overall"}.
Правки минимальные: не переписывай файлы целиком, old должен совпадать буквально.\
Если править нечего — сразу done."""


class Dream:
    def __init__(
        self,
        workspace: Workspace,
        gitstore: GitStore,
        llm: LLMProtocol | None,
        settings: Settings,
        db: Database,
        graph: Any = None,  # GraphService | None: сессии -> общий граф (dream-extract)
    ):
        self.workspace = workspace
        self.gitstore = gitstore
        self.llm = llm
        self.settings = settings
        self.db = db
        self.graph = graph

    # --- запуск ---

    def run(self, trigger: str = "manual") -> dict[str, Any]:
        started = utcnow()
        cur = self.db.execute(
            "INSERT INTO dream_runs (started_at, status, trigger) VALUES (?,?,?)",
            (started, "running", trigger),
        )
        run_id = cur.lastrowid
        try:
            stats = self._dream()
            self._finish(run_id, "ok", stats)
            return {"run_id": run_id, "status": "ok", **stats}
        except Exception as e:
            log.exception("dream run failed")
            self._finish(run_id, "error", {"error": str(e)})
            return {"run_id": run_id, "status": "error", "error": str(e)}

    def _finish(self, run_id: int, status: str, stats: dict) -> None:
        self.db.execute(
            "UPDATE dream_runs SET finished_at=?, status=?, stats=? WHERE id=?",
            (utcnow(), status, json.dumps(stats, ensure_ascii=False), run_id),
        )

    # --- ядро ---

    def _dream(self) -> dict[str, Any]:
        if self.llm is None:
            raise RuntimeError("LLM не настроен (OMNES_LLM_API_KEY)")

        cursor = self.workspace.get_cursor("dream_cursor")
        new_records = [
            r for r in self.workspace.load_history() if r.get("cursor", 0) > cursor
        ][: self.settings.dream_batch]
        if not new_records:
            return {"processed": 0, "edits": 0, "commit": None,
                    "note": "нет новых записей с прошлого дрима"}

        analysis = self._phase1(new_records)
        edits = self._phase2(analysis)
        graph_stats = self._extract_to_graph(new_records)
        new_cursor = max(r.get("cursor", 0) for r in new_records)
        self.workspace.set_cursor("dream_cursor", new_cursor)
        self.workspace.compact_history()
        commit = self.gitstore.auto_commit(
            f"dream: {datetime.now():%Y-%m-%d %H:%M} (+{len(edits)} правок)"
        )
        stats: dict[str, Any] = {
            "processed": len(new_records), "edits": len(edits), "commit": commit
        }
        if graph_stats is not None:
            stats.update(graph_stats)
        return stats

    def _extract_to_graph(self, records: list[dict]) -> dict[str, int] | None:
        """Извлечение сущностей из новых записей сессий в общий граф.

        Один граф на владельца: узлы из сессий и из документов дедуплицируются
        по (label, type) — упоминание в диалоге увеличивает val существующего узла.
        Отдельная фаза дрима, выключается OMNES_DREAM_EXTRACT_ENABLED=false.
        """
        if self.graph is None or not self.settings.dream_extract_enabled:
            return None
        text = "\n".join(str(r.get("content", ""))[:1500] for r in records)[:15000]
        if len(text.strip()) < 80:
            return {"graph_entities": 0, "graph_edges": 0}
        from .extractor import Extractor

        try:
            result = Extractor(self.llm, max_chunks=30).extract(text)
        except Exception as e:  # сбой экстракции не роняет дрим
            log.warning("dream-extract не удался: %s", e)
            return {"graph_entities": 0, "graph_edges": 0, "graph_error": str(e)[:200]}
        upsert = self.graph.upsert_extraction(result)
        return {
            "graph_entities": upsert["new_entities"] + upsert["updated_entities"],
            "graph_edges": upsert["new_edges"],
        }

    def _phase1(self, records: list[dict]) -> str:
        history = "\n".join(
            f"[{r.get('timestamp', '')}] {r.get('content', '')[:800]}" for r in records
        )
        files = "\n\n".join(
            f"=== {name.upper()}.md ===\n{self.workspace.read(name)}"
            for name in ("memory", "soul", "user")
        )
        try:
            return self.llm.chat(
                [{"role": "system", "content": PHASE1_SYSTEM},
                 {"role": "user", "content": content_prompt(history, files)}],
                temperature=0.2,
            )
        except LLMError as e:
            raise RuntimeError(f"фаза 1 не удалась: {e}") from e

    def _phase2(self, analysis: str) -> list[dict[str, Any]]:
        """Агентный цикл правок (порт фазы 2 Dream). Возвращает применённые правки."""
        applied: list[dict[str, Any]] = []
        context_files = {name: self.workspace.read(name)
                         for name in ("memory", "soul", "user")}
        last_error = ""
        for _ in range(MAX_ITERATIONS):
            files_block = "\n\n".join(
                f"=== {n}.md ===\n{c}" for n, c in context_files.items()
            )
            prompt = f"Анализ:\n{analysis}\n\n{files_block}"
            if last_error:
                prompt += f"\n\nОшибка прошлого шага (исправь): {last_error}"
            prompt += '\n\nТвоё действие (JSON):'
            try:
                action = self.llm.ask_json(PHASE2_SYSTEM, prompt, temperature=0.1)
            except LLMError as e:
                log.warning("фаза 2: LLM прервался: %s", e)
                break
            if not isinstance(action, dict):
                last_error = "ожидался JSON-объект действия"
                continue
            act = action.get("action")
            if act == "done":
                break
            if act == "read" and action.get("file") in context_files:
                name = action["file"]
                context_files[name] = self.workspace.read(name)
                last_error = ""
                continue
            if act == "edit":
                result, err = self._apply_edit(action)
                if err:
                    last_error = err
                    continue
                applied.append(result)
                context_files[result["file"]] = self.workspace.read(result["file"])
                last_error = ""
                continue
            last_error = f"неизвестное действие {act!r}"
        return applied

    def _apply_edit(self, action: dict) -> tuple[dict[str, Any], str]:
        name = action.get("file")
        if name not in ("memory", "soul", "user"):
            return {}, "file должен быть memory|soul|user"
        old, new = str(action.get("old", "")), str(action.get("new", ""))
        if not old or not new or old == new:
            return {}, "пустые или одинаковые old/new"
        content = self.workspace.read(name)
        if old not in content:
            return {}, f"фрагмент не найден в {name}.md (проверь дословно)"
        self.workspace.write(name, content.replace(old, new, 1))
        return {"file": name, "file_path": f"{name}.md",
                "old": old[:100], "new": new[:100]}, ""


def content_prompt(history: str, files: str) -> str:
    return f"Новые записи истории:\n{history}\n\n{files}"
