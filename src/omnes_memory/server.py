"""MCP-сервер OmnesMemory (stdio). Инструменты памяти и workspace (фаза 2).

Правило ошибок (AGENTS.md §6): инструмент возвращает строку "[Error] ...", не бросает.
Логи: logs/omnes-memory.log с ротацией.
"""

from __future__ import annotations

import functools
import logging
import logging.handlers
from pathlib import Path
from typing import Any

from mcp.server.mcpserver import MCPServer

from .config import Settings, get_settings
from .db import Database
from .embedding import provider_for
from .gitstore import GitStore
from .memory_service import MemoryService
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
    setup_logging(get_settings())
    logging.getLogger("omnes").info("OmnesMemory MCP-сервер запускается (stdio)")
    mcp.run()


if __name__ == "__main__":
    main()
