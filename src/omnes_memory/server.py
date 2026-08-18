"""MCP-сервер OmnesMemory (stdio). Инструменты памяти и workspace (фаза 2).

Правило ошибок (AGENTS.md §6): инструмент возвращает строку "[Error] ...", не бросает.
Логи: logs/omnes-memory.log с ротацией.
"""

from __future__ import annotations

import functools
import json
import logging
import logging.handlers
import threading
from pathlib import Path
from typing import Any

from mcp.server.mcpserver import MCPServer

from .config import Settings, get_settings
from .consolidator import Consolidator, PendingSession
from .db import Database, utcnow
from .dream import Dream
from .embedding import provider_for
from .extractor import Extractor, split_into_chunks
from .gitstore import GitStore
from .graph_service import GraphService
from .llm_client import make_llm
from .memory_service import MemoryService
from .vector import serialize
from .workspace import Workspace


def setup_logging(settings: Settings) -> None:
    settings.logs_dir.mkdir(parents=True, exist_ok=True)
    handler = logging.handlers.RotatingFileHandler(
        settings.logs_dir / "omnes-memory.log",
        maxBytes=5 * 1024 * 1024,
        backupCount=5,
        encoding="utf-8",
    )
    fmt = logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s")
    handler.setFormatter(fmt)
    root = logging.getLogger()
    root.setLevel(settings.log_level.upper())
    root.addHandler(handler)
    # stdio занят протоколом MCP — в консоль не пишем


class App:
    """Контекст сервера: одно подключение к SQLite, один провайдер эмбеддингов."""

    def __init__(self, settings: Settings):
        self.settings = settings
        settings.ensure_dirs()
        self.db = Database(settings.db_path)
        self.embedder = provider_for(settings)
        self.memory = MemoryService(self.db, self.embedder)
        self.workspace = Workspace(settings.workspace_dir)
        self.gitstore = GitStore(settings.workspace_dir)
        self.llm = make_llm(settings)
        self.consolidator = Consolidator(self.workspace, self.llm, settings)
        self.pending_session = PendingSession()
        self.graph = GraphService(self.db, self.embedder)
        self.dream = Dream(self.workspace, self.gitstore, self.llm, settings, self.db)
        self.dream_lock = threading.Lock()
        self.autodream = None  # запускается в main() при OMNES_AUTODREAM_ENABLED

    def start_background_workers(self) -> None:
        if self.settings.autodream_enabled:
            from .autodream import AutoDreamWorker

            self.autodream = AutoDreamWorker(self.dream, self.workspace, self.settings)
            self.autodream.start()


@functools.lru_cache(maxsize=1)
def get_app() -> App:
    return App(get_settings())


mcp = MCPServer("omnes-memory")


def truncate(text: str, limit: int | None = None) -> str:
    limit = limit or get_app().settings.max_tool_output_chars
    if len(text) <= limit:
        return text
    return text[:limit] + "…[truncated]"


def _fmt_hit(i: int, m: dict[str, Any]) -> str:
    score = m.get("rrf_score") or m.get("vector_score") or m.get("fts_rank")
    return (
        f"[{i}] key={m['key']} cat={m.get('category', '-')} "
        f"imp={m.get('importance', 0):.2f} score={score} | {m['content'][:200]}"
    )


# ── Память ──────────────────────────────────────────────────────────────


@mcp.tool()
def memory_save(
    content: str,
    key: str | None = None,
    category: str = "general",
    importance: float = 0.5,
    source: str = "chat",
) -> str:
    """Сохранить факт в долгосрочную память. key опционален (сгенерируется).
    importance 0..1 — насколько важно помнить. category — произвольная метка."""
    try:
        result = get_app().memory.upsert(
            content=content, key=key, category=category,
            importance=max(0.0, min(1.0, importance)), source=source,
        )
        return f"{result['status']} key={result['key']}"
    except Exception as e:
        logging.getLogger("omnes.tools").exception("memory_save")
        return f"[Error] {e}"


@mcp.tool()
def memory_search(query: str, limit: int = 5, mode: str = "hybrid") -> str:
    """Поиск по памяти: hybrid (по умолчанию, FTS+вектор RRF) | fts | vector."""
    try:
        app = get_app()
        mode = mode if mode in ("hybrid", "fts", "vector") else "hybrid"
        if mode == "fts":
            hits = app.memory.search_fts(query, limit=limit)
        elif mode == "vector":
            hits = app.memory.search_vector(query, limit=limit)
        else:
            hits = app.memory.search_hybrid(query, limit=limit)
        if not hits:
            return "ничего не найдено"
        return "\n".join(_fmt_hit(i, m) for i, m in enumerate(hits, 1))
    except Exception as e:
        logging.getLogger("omnes.tools").exception("memory_search")
        return f"[Error] {e}"


@mcp.tool()
def memory_update(key: str, content: str | None = None,
                  importance: float | None = None,
                  category: str | None = None) -> str:
    """Обновить воспоминание по ключу (любое из полей)."""
    try:
        status = get_app().memory.update(
            key, content=content, importance=importance, category=category
        )
        return f"{status} key={key}"
    except Exception as e:
        logging.getLogger("omnes.tools").exception("memory_update")
        return f"[Error] {e}"


@mcp.tool()
def memory_forget(key: str) -> str:
    """Удалить воспоминание по ключу."""
    try:
        return f"{get_app().memory.forget(key)} key={key}"
    except Exception as e:
        logging.getLogger("omnes.tools").exception("memory_forget")
        return f"[Error] {e}"


@mcp.tool()
def memory_context(query: str = "", max_tokens: int = 1000) -> str:
    """Блок <agent_memory> с самыми важными фактами — для вставки в промпт.
    query повышает релевантность отбора."""
    try:
        return get_app().memory.build_context(query=query, max_tokens=max_tokens)
    except Exception as e:
        logging.getLogger("omnes.tools").exception("memory_context")
        return f"[Error] {e}"


# ── Workspace ───────────────────────────────────────────────────────────


@mcp.tool()
def workspace_read(file: str) -> str:
    """Прочитать файл агента: memory (MEMORY.md) | soul (SOUL.md) | user (USER.md)
    | history (консолидированная история, jsonl)."""
    try:
        return truncate(get_app().workspace.read(file))
    except Exception as e:
        logging.getLogger("omnes.tools").exception("workspace_read")
        return f"[Error] {e}"


@mcp.tool()
def workspace_write(file: str, content: str, commit_message: str = "") -> str:
    """Перезаписать файл агента (memory|soul|user) с git-коммитом."""
    try:
        app = get_app()
        rel = app.workspace.write(file, content)
        sha = app.gitstore.auto_commit(
            commit_message or f"agent write: {file}"
        )
        return f"written {rel}" + (f" commit={sha}" if sha else " (no git changes)")
    except Exception as e:
        logging.getLogger("omnes.tools").exception("workspace_write")
        return f"[Error] {e}"


# ── Сессии ──────────────────────────────────────────────────────────────


@mcp.tool()
def session_log(user_text: str, assistant_text: str, source: str = "hermes") -> str:
    """Залогировать ход диалога после ответа агента. Пишет событие в daily-лог
    (пища для дрима) и при переполнении бюджета токенов консолидирует итог
    в history.jsonl. Вызывай после каждого значимого обмена."""
    try:
        app = get_app()
        from .db import utcnow

        app.workspace.append_daily_event({
            "timestamp": utcnow(),
            "query": user_text[:500],
            "answer_preview": assistant_text[:240],
            "source": source,
        })
        app.pending_session.append("user", user_text)
        app.pending_session.append("assistant", assistant_text)
        result = app.consolidator.maybe_consolidate(app.pending_session)
        status = "logged"
        if result["consolidated"]:
            status += f" +consolidated x{result['entries']}"
        return status
    except Exception as e:
        logging.getLogger("omnes.tools").exception("session_log")
        return f"[Error] {e}"


# ── Граф знаний ─────────────────────────────────────────────────────────


@mcp.tool()
def knowledge_extract(
    text: str | None = None,
    file_path: str | None = None,
    max_chunks: int = 200,
) -> str:
    """Извлечь сущности и отношения из текста или файла (txt/md/pdf/docx)
    в граф знаний. Один из аргументов text/file_path обязателен."""
    try:
        app = get_app()
        if not text and not file_path:
            return "[Error] укажите text или file_path"
        meta: dict = {}
        title = "текст из чата"
        if file_path:
            from .ingest import read_document

            text, meta = read_document(file_path)
            title = meta.get("file_name", file_path)
        if not text or not text.strip():
            return "[Error] пустой текст"

        doc_cur = app.db.execute(
            "INSERT INTO documents (title, path, meta, created_at) VALUES (?,?,?,?)",
            (title, file_path, json.dumps(meta, ensure_ascii=False),
             utcnow()),
        )
        doc_id = doc_cur.lastrowid

        chunks = split_into_chunks(text)[:max_chunks]
        chunk_vecs = app.embedder.embed(chunks) if chunks else []
        for ordinal, (chunk, vec) in enumerate(zip(chunks, chunk_vecs, strict=False)):
            app.db.execute(
                "INSERT INTO chunks (doc_id, ordinal, text, embedding, created_at) "
                "VALUES (?,?,?,?,?)",
                (doc_id, ordinal, chunk, serialize(vec), utcnow()),
            )

        if app.llm is None:
            return ("[Error] LLM не настроен (OMNES_LLM_API_KEY) — извлечение "
                    f"невозможно; документ и {len(chunks)} чанков сохранены doc_id={doc_id}")

        extractor = Extractor(app.llm, max_chunks=max_chunks)
        result = extractor.extract(text)
        stats = app.graph.upsert_extraction(result)
        return (
            f"doc_id={doc_id} chunks={result.chunks_processed}"
            f"(+{result.chunks_skipped} пропущено) "
            f"entities={stats['new_entities']}новых+{stats['updated_entities']}дублей "
            f"relations={stats['new_edges']}новых"
        )
    except Exception as e:
        logging.getLogger("omnes.tools").exception("knowledge_extract")
        return f"[Error] {e}"


@mcp.tool()
def graph_search(query: str, limit: int = 10) -> str:
    """Поиск по графу знаний: узлы и связи (с 1-hop соседями)."""
    try:
        found = get_app().graph.search(query, limit=limit)
        if not found["nodes"]:
            return "граф пуст по запросу"
        lines = [f"узлов: {len(found['nodes'])}, связей: {len(found['edges'])}"]
        for n in found["nodes"][:limit]:
            desc = f" — {n['description'][:120]}" if n["description"] else ""
            lines.append(f"- {n['label']} ({n['node_type']}, val={n['val']}){desc}")
        for e in found["edges"][:limit * 2]:
            lines.append(f"- {e['source_label']} --[{e['label']}]--> {e['target_label']}")
        return truncate("\n".join(lines))
    except Exception as e:
        logging.getLogger("omnes.tools").exception("graph_search")
        return f"[Error] {e}"


@mcp.tool()
def graph_reason(query: str) -> str:
    """Ответ по графу знаний с уверенностью и цепочкой рассуждения (KAG)."""
    try:
        app = get_app()
        if app.llm is None:
            return "[Error] LLM не настроен (OMNES_LLM_API_KEY)"
        answer = app.graph.reason(query, app.llm)
        steps = "; ".join(answer.get("reasoning_steps", []))
        used = ", ".join(answer.get("used_entities", []))
        return truncate(
            f"answer: {answer.get('answer')}\n"
            f"confidence: {answer.get('confidence')}\n"
            f"entities: {used or '-'}\n"
            f"steps: {steps or '-'}"
        )
    except Exception as e:
        logging.getLogger("omnes.tools").exception("graph_reason")
        return f"[Error] {e}"


@mcp.tool()
def graph_stats() -> str:
    """Статистика графа знаний: узлы, связи, документы, чанки."""
    try:
        s = get_app().graph.stats()
        return (f"nodes={s['nodes']} edges={s['edges']} "
                f"documents={s['documents']} chunks={s['chunks']}")
    except Exception as e:
        logging.getLogger("omnes.tools").exception("graph_stats")
        return f"[Error] {e}"


# ── Дриминг ─────────────────────────────────────────────────────────────


@mcp.tool()
def dream_run(background: bool = False) -> str:
    """Запустить дрим: анализ новой истории и правки MEMORY/SOUL/USER с git-коммитом.
    background=false ждёт завершения (может занять минуты)."""
    try:
        app = get_app()
        if not app.dream_lock.acquire(blocking=False):
            return "дрим уже выполняется"
        try:
            if background:
                threading.Thread(
                    target=_dream_bg, args=(app,), daemon=True, name="omnes-dream",
                ).start()
                return "дрим запущен в фоне — статус: dream_status"
            result = app.dream.run(trigger="manual")
            if result["status"] == "error":
                return f"[Error] {result.get('error')}"
            return (f"run_id={result['run_id']} processed={result['processed']} "
                    f"edits={result['edits']} commit={result.get('commit') or '-'}"
                    + (f" ({result['note']})" if result.get("note") else ""))
        finally:
            if not background:
                app.dream_lock.release()
    except Exception as e:
        logging.getLogger("omnes.tools").exception("dream_run")
        return f"[Error] {e}"


def _dream_bg(app: App) -> None:
    try:
        app.dream.run(trigger="manual-bg")
    finally:
        app.dream_lock.release()


@mcp.tool()
def dream_status() -> str:
    """Статус дрима: последний запуск, состояние гейтов автодрима."""
    try:
        app = get_app()
        row = app.db.query_one(
            "SELECT id, started_at, finished_at, status, trigger, stats "
            "FROM dream_runs ORDER BY id DESC LIMIT 1"
        )
        lines = []
        if row:
            lines.append(
                f"last_run: id={row['id']} {row['status']} ({row['trigger']}) "
                f"{row['started_at']} -> {row['finished_at']}"
            )
            if row["stats"]:
                lines.append(f"stats: {row['stats'][:300]}")
        else:
            lines.append("last_run: никогда")
        lines.append(f"dream_cursor: {app.workspace.get_cursor('dream_cursor')}")
        if app.autodream is not None:
            ok, reason = app.autodream.should_run()
            lines.append(f"autodream: {'готов' if ok else 'ждёт'} ({reason})")
        else:
            lines.append("autodream: выключен")
        return "\n".join(lines)
    except Exception as e:
        logging.getLogger("omnes.tools").exception("dream_status")
        return f"[Error] {e}"


@mcp.tool()
def dream_log(limit: int = 10) -> str:
    """История dream-коммитов в git-репозитории workspace."""
    try:
        entries = get_app().gitstore.log(limit=limit)
        if not entries:
            return "коммитов нет"
        return "\n".join(
            f"{e['sha']} {e['date']} {e['message']}" for e in entries
        )
    except Exception as e:
        logging.getLogger("omnes.tools").exception("dream_log")
        return f"[Error] {e}"


@mcp.tool()
def dream_restore(commit: str) -> str:
    """Откатить MEMORY/SOUL/USER к состоянию коммита (sha из dream_log)."""
    try:
        return get_app().gitstore.restore(commit)
    except Exception as e:
        logging.getLogger("omnes.tools").exception("dream_restore")
        return f"[Error] {e}"


# ── Служебное ───────────────────────────────────────────────────────────


@mcp.tool()
def omnes_stats() -> str:
    """Статистика хранилища: памяти, графа, документов, дримов."""
    try:
        app = get_app()
        db = app.db

        def count(table: str) -> int:
            return db.query_one(f"SELECT count(*) AS c FROM {table}")["c"]  # noqa: S608

        db_size = Path(app.settings.db_path).stat().st_size if app.settings.db_path.exists() else 0
        parts = [
            f"memories={count('memories')}",
            f"relations={count('memory_relations')}",
            f"documents={count('documents')}",
            f"chunks={count('chunks')}",
            f"graph_nodes={count('graph_nodes')}",
            f"graph_edges={count('graph_edges')}",
            f"dream_runs={count('dream_runs')}",
            f"db={db_size / 1024:.0f}KB",
            f"history={len(app.workspace.load_history())}",
        ]
        return " ".join(parts)
    except Exception as e:
        logging.getLogger("omnes.tools").exception("omnes_stats")
        return f"[Error] {e}"


def main() -> None:
    settings = get_settings()
    setup_logging(settings)
    app = get_app()
    app.start_background_workers()
    logging.getLogger("omnes").info("OmnesMemory MCP-сервер запускается (stdio)")
    mcp.run()


if __name__ == "__main__":
    main()
