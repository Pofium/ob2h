"""OB2H — MemoryProvider для Hermes (обёртка над Rust-бинарником ``ob2h``).

Архитектура (docs/PLAN_v0.8.md, фаза 7): плагин держит долгоживущий subprocess
``ob2h serve`` и говорит с ним JSON-RPC поверх stdio. Автоматически, без
инициативы модели:

- ``sync_turn``  → каждый ход диалога пишется в daily-лог (session_ingest);
- ``prefetch``   → перед ходом инжектится блок ``<agent_memory>`` (гибридный поиск);
- ``on_session_end`` / ``on_pre_compress`` → полная транскрипта для дрима;
- ``get_tool_schemas`` → инструменты графа/дрима/памяти ob2h.

Только stdlib. Конфиг: ``$HERMES_HOME/ob2h.json`` ({"binary": ..., "data_dir": ...})
или env ``OB2H_BIN`` / ``OB2H_DATA_DIR``.
"""

from __future__ import annotations

import json
import logging
import os
import queue
import shutil
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

try:
    from agent.memory_provider import MemoryProvider, RecallStatus
except ImportError:  # импорт вне рантайма Hermes (юнит-тесты) — база подменяется стабом
    MemoryProvider = object  # type: ignore[assignment,misc]
    RecallStatus = None  # type: ignore[assignment]

from ._rpc import Ob2hRpc, RpcError

logger = logging.getLogger(__name__)

# Инструменты, которые плагин НЕ экспонирует модели:
# session_log/session_ingest — автоматические (sync_turn), memory_search/memory_context —
# замещены prefetch-инъекцией.
_EXPOSE_EXCLUDE = {"session_log", "session_ingest", "memory_search", "memory_context"}

_PREFETCH_TIMEOUT = 10.0
_WRITE_TIMEOUT = 120.0

# Windows-путь разработки — последний кандидат (для VPS бинарник берётся из PATH/env).
_DEV_BINARY_FALLBACK = r"C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe"


def _load_cfg(hermes_home: str) -> Dict[str, str]:
    path = Path(hermes_home) / "ob2h.json"
    if path.is_file():
        try:
            data = json.loads(path.read_text(encoding="utf-8-sig"))
            return {k: str(v) for k, v in data.items() if isinstance(v, (str, int, float))}
        except Exception as e:
            logger.warning("ob2h: не удалось прочитать %s: %s", path, e)
    return {}


def _binary_candidates(cfg: Dict[str, str]) -> List[str]:
    out: List[str] = []
    if cfg.get("binary"):
        out.append(cfg["binary"])
    if os.environ.get("OB2H_BIN"):
        out.append(os.environ["OB2H_BIN"])
    which = shutil.which("ob2h")
    if which:
        out.append(which)
    if os.name == "nt":
        out.append(_DEV_BINARY_FALLBACK)
    return out


def _read_dotenv(path: Path) -> Dict[str, str]:
    out: Dict[str, str] = {}
    try:
        for line in path.read_text(encoding="utf-8-sig").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip().strip('"').strip("'")
    except Exception:
        pass
    return out


def _llm_child_env(hermes_home: str, cfg: Dict[str, str]) -> Dict[str, str]:
    """Собрать OB2H_LLM_* для дочернего процесса ``ob2h serve``.

    Автоподстановка ключа агента, к которому подключён ob2h: если явных
    OB2H_LLM_* нигде нет, берём ключ/модель/URL агента (его ``.env``
    в $HERMES_HOME/.env, ключ по конвенции DEEPSEEK_API_KEY). Без этого
    ob2h в режиме плагина остаётся без ключа и дриминг/извлечение падают
    с 401. Приоритет источников: окружение Hermes > ob2h.json > .env Hermes."""
    dot = _read_dotenv(Path(hermes_home) / ".env")

    def pick(name, default=None):
        for src in (os.environ, cfg, dot):
            v = src.get(name)
            if v:
                return v
        return default

    key = pick("OB2H_LLM_API_KEY")
    if not key:
        agent_key = os.environ.get("DEEPSEEK_API_KEY") or dot.get(
            "DEEPSEEK_API_KEY"
        )
        if agent_key:
            key = agent_key  # резолвленный литерал — индирекция не нужна

    out: Dict[str, str] = {}
    if key:
        out["OB2H_LLM_API_KEY"] = key
    model = pick("OB2H_LLM_MODEL", "deepseek-v4-flash")
    if model:
        out["OB2H_LLM_MODEL"] = model
    base = pick("OB2H_LLM_BASE_URL", "https://api.deepseek.com/v1")
    if base:
        out["OB2H_LLM_BASE_URL"] = base
    return out


class Ob2hProvider(MemoryProvider):
    """MemoryProvider поверх ``ob2h serve``."""

    def __init__(self) -> None:
        self._hermes_home = ""
        self._cfg: Dict[str, str] = {}
        self._binary = ""
        self._unavailable = ""
        self._writes_enabled = True
        self._session_id = ""
        # Аккумулятор сообщений текущей сессии: каждый ход отправляется полным
        # префиксом, сервер пишет только хвост (дедуп по session_id/позиции).
        self._accumulated: List[Dict[str, str]] = []
        self._rpc: Optional[Ob2hRpc] = None
        self._write_q: "queue.Queue[Optional[tuple]]" = queue.Queue()
        self._writer_thread: Optional[threading.Thread] = None
        self._stop = threading.Event()
        self._tool_schemas: List[Dict[str, Any]] = []
        self._prefetch_cache: Dict[str, Tuple[str, int]] = {}
        self._prefetch_lock = threading.Lock()
        self._last_recall_count = 0
        self._data_dir = ""

    # -- доступность и конфиг -------------------------------------------------

    @property
    def name(self) -> str:
        return "ob2h"

    def _resolve_binary(self) -> bool:
        self._cfg = _load_cfg(self._hermes_home)
        for cand in _binary_candidates(self._cfg):
            if shutil.which(cand):  # which понимает и полный путь, и имя в PATH
                self._binary = cand
                self._unavailable = ""
                return True
        self._unavailable = (
            "бинарник ob2h не найден: укажите OB2H_BIN, ob2h.json {\"binary\": ...} "
            "или положите ob2h в PATH"
        )
        return False

    def is_available(self) -> bool:
        return self._resolve_binary()

    def unavailable_reason(self) -> str:
        return self._unavailable

    def get_config_schema(self) -> List[Dict[str, Any]]:
        return [
            {
                "key": "binary",
                "description": "Полный путь к ob2h(.exe) (пусто = PATH/OB2H_BIN)",
                "type": "text",
                "required": False,
            },
            {
                "key": "data_dir",
                "description": "OB2H_DATA_DIR (папка data/ проекта ob2h)",
                "type": "text",
                "required": False,
            },
        ]

    def save_config(self, values: Dict[str, Any], hermes_home: str) -> None:
        path = Path(hermes_home) / "ob2h.json"
        current: Dict[str, Any] = {}
        if path.is_file():
            try:
                current = json.loads(path.read_text(encoding="utf-8-sig"))
            except Exception:
                current = {}
        for key in ("binary", "data_dir"):
            if key in values and values[key]:
                current[key] = values[key]
        path.write_text(json.dumps(current, ensure_ascii=False, indent=2), encoding="utf-8")

    def backup_paths(self) -> List[str]:
        return [self._data_dir] if self._data_dir else []

    # -- lifecycle -------------------------------------------------------------

    def initialize(self, session_id: str, **kwargs) -> None:
        self._hermes_home = kwargs.get("hermes_home") or os.environ.get(
            "HERMES_HOME", str(Path.home() / ".hermes")
        )
        self._session_id = session_id or ""
        # Крон/subagent-контексты не пишем — их сисколлы замусорили бы память.
        self._writes_enabled = kwargs.get("agent_context", "primary") == "primary"

        if not self._resolve_binary():
            logger.warning("ob2h: %s", self._unavailable)
            return

        self._data_dir = self._cfg.get("data_dir") or os.environ.get("OB2H_DATA_DIR", "")
        env = {"OB2H_DATA_DIR": self._data_dir} if self._data_dir else {}
        # Автоподстановка ключа агента (Hermes), чтобы ob2h-плагин не оставался
        # без ключа: иначе дриминг/LLM-инструменты падают с 401.
        env.update(_llm_child_env(self._hermes_home, self._cfg))
        self._rpc = Ob2hRpc([self._binary, "serve"], env=env)

        try:
            self._rpc.start()
            self._refresh_tool_schemas()
        except Exception as e:
            # Деградация: prefetch/write-потоки сами поднимут процесс (ensure + backoff).
            logger.warning("ob2h: старт не удался, работаем в режиме восстановления: %s", e)

        self._writer_thread = threading.Thread(
            target=self._writer_loop, daemon=True, name="ob2h-writer"
        )
        self._writer_thread.start()
        threading.Thread(target=self._health_loop, daemon=True, name="ob2h-health").start()

    def _refresh_tool_schemas(self) -> None:
        tools = self._rpc.tools_list()
        self._tool_schemas = [
            {
                "name": t["name"],
                "description": t.get("description", ""),
                "parameters": t.get("input_schema", {"type": "object"}),
            }
            for t in tools
            if t.get("name") and t["name"] not in _EXPOSE_EXCLUDE
        ]
        logger.info("ob2h: экспонировано %d инструментов", len(self._tool_schemas))

    def shutdown(self) -> None:
        self._stop.set()
        self._write_q.put(None)
        if self._writer_thread:
            self._writer_thread.join(timeout=3.0)
        if self._rpc:
            self._rpc.stop()

    # -- recall (детерминированная инъекция памяти) -----------------------------

    def system_prompt_block(self) -> str:
        return (
            "Долговременная память ob2h подключена. Релевантные воспоминания "
            "автоматически всплывают блоками <agent_memory> перед каждым ходом — "
            "опирайся на них. Явные операции: ob2h-инструменты памяти, графа знаний "
            "и дриминга (memory_save, knowledge_extract, graph_reason, dream_*)."
        )

    def queue_prefetch(self, query: str, *, session_id: str = "") -> None:
        sid = session_id or self._session_id
        threading.Thread(
            target=self._do_prefetch, args=(query, sid), daemon=True,
            name="ob2h-prefetch",
        ).start()

    def _do_prefetch(self, query: str, sid: str) -> None:
        result: Tuple[str, int] = ("", 0)
        try:
            if self._rpc:
                self._rpc.ensure()
                block = self._rpc.tool_call(
                    "memory_context", {"query": query, "max_tokens": 30},
                    timeout=_PREFETCH_TIMEOUT,
                ).strip()
                if block:
                    lines = [
                        l for l in block.splitlines()
                        if l.strip() and not l.strip().startswith("<")
                    ]
                    result = (block, len(lines))
                else:
                    hits = self._rpc.tool_call(
                        "memory_search", {"query": query, "limit": 8},
                        timeout=_PREFETCH_TIMEOUT,
                    ).strip()
                    if hits and "ничего не найдено" not in hits:
                        n = len([l for l in hits.splitlines() if l.strip()])
                        result = (f"<agent_memory>\n{hits}\n</agent_memory>", n)
        except Exception as e:
            logger.debug("ob2h: prefetch не удался: %s", e)
        with self._prefetch_lock:
            self._prefetch_cache[sid] = result

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        sid = session_id or self._session_id
        with self._prefetch_lock:
            cached = self._prefetch_cache.pop(sid, ("", 0))
        self._last_recall_count = cached[1]
        return cached[0]

    def recall_status(self):
        if RecallStatus is None or self._last_recall_count == 0:
            return None
        return RecallStatus(provider_label="ob2h", count=self._last_recall_count, glyph="🧠")

    # -- запись (автозахват) ----------------------------------------------------

    def sync_turn(
        self,
        user_content: str,
        assistant_content: str,
        *,
        session_id: str = "",
        messages: Optional[List[Dict[str, Any]]] = None,
    ) -> None:
        if not self._writes_enabled:
            return
        if not (user_content or "").strip() and not (assistant_content or "").strip():
            return
        sid = session_id or self._session_id
        if sid:
            self._session_id = sid
            self._accumulated.append({"role": "user", "content": user_content or ""})
            self._accumulated.append({"role": "assistant", "content": assistant_content or ""})
            # Полный префикс: сервер пропустит уже принятые сообщения (дедуп).
            self._write_q.put(("ingest", list(self._accumulated), "hermes", sid))
        else:
            # Без session_id дедупа нет — ход пишется напрямую.
            self._write_q.put(("log", user_content or "", assistant_content or ""))

    def on_session_end(self, messages: List[Dict[str, Any]]) -> None:
        if not self._writes_enabled or not messages:
            return
        msgs = self._normalize(messages)
        if not msgs:
            return
        sid = self._session_id
        if sid:
            self._accumulated = msgs
        self._write_q.put(("ingest", msgs, "hermes", sid))

    def on_session_switch(self, new_session_id: str = "", **kwargs) -> None:
        self._session_id = new_session_id or ""
        self._accumulated = []
        with self._prefetch_lock:
            self._prefetch_cache.clear()

    def on_pre_compress(self, messages: List[Dict[str, Any]]) -> str:
        """Спасти контент до сжатия контекста: отдельный pseudo-session_id,
        чтобы позиционный дедуп основной сессии не разошёлся."""
        if not self._writes_enabled or not messages:
            return ""
        msgs = self._normalize(messages)
        if not msgs:
            return ""
        sid = (self._session_id or "session") + ":precompress"
        self._write_q.put(("ingest", msgs, "pre_compress", sid))
        return ""

    def on_memory_write(
        self,
        action: str,
        target: str,
        content: str,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> None:
        """Зеркалим записи встроенной памяти Hermes (если пользователь её вызвал)."""
        if action == "add" and content and self._writes_enabled:
            self._write_q.put(("save", content))

    # -- инструменты -------------------------------------------------------------

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        return self._tool_schemas

    def handle_tool_call(self, tool_name: str, args: Dict[str, Any], **kwargs) -> str:
        try:
            if not self._rpc:
                return "[Error] ob2h не инициализирован (бинарник не найден?)"
            self._rpc.ensure()
            out = self._rpc.tool_call(tool_name, args)
            return out if out.strip() else "(пусто)"
        except RpcError as e:
            return f"[Error] {e}"

    # -- внутренние потоки ---------------------------------------------------------

    @staticmethod
    def _normalize(messages: List[Dict[str, Any]]) -> List[Dict[str, str]]:
        out: List[Dict[str, str]] = []
        for m in messages:
            role = m.get("role")
            content = m.get("content")
            if role in ("user", "assistant") and isinstance(content, str) and content.strip():
                out.append({"role": role, "content": content})
        return out

    def _writer_loop(self) -> None:
        while not self._stop.is_set():
            item = self._write_q.get()
            try:
                if item is None:
                    break
                try:
                    if self._rpc:
                        self._rpc.ensure()
                        kind = item[0]
                        if kind == "ingest":
                            _, msgs, source, sid = item
                            out = self._rpc.tool_call(
                                "session_ingest",
                                {"messages": msgs, "source": source, "session_id": sid},
                                timeout=_WRITE_TIMEOUT,
                            )
                            logger.debug("ob2h ingest: %s", out)
                        elif kind == "log":
                            _, user_text, assistant_text = item
                            self._rpc.tool_call(
                                "session_log",
                                {"user_text": user_text, "assistant_text": assistant_text},
                                timeout=_WRITE_TIMEOUT,
                            )
                        elif kind == "save":
                            _, content = item
                            self._rpc.tool_call(
                                "memory_save",
                                {"content": content, "source": "hermes-builtin", "importance": 0.7},
                                timeout=_WRITE_TIMEOUT,
                            )
                except Exception as e:
                    # Потеря одного хода не критична (дрим терпит дыры); не роняем поток.
                    logger.warning("ob2h: запись не удалась: %s", e)
                    time.sleep(1.0)
            finally:
                self._write_q.task_done()

    def _health_loop(self) -> None:
        while not self._stop.wait(60.0):
            if self._rpc and not self._rpc.alive():
                try:
                    self._rpc.ensure()
                except Exception as e:
                    logger.debug("ob2h: health-рестарт отложен: %s", e)


__all__ = ["Ob2hProvider"]
