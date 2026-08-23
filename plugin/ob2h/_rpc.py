"""JSON-RPC 2.0-клиент поверх stdio-процесса ``ob2h serve``.

Только stdlib. Потокобезопасен: один писатель (lock), один читатель (тред),
ждущие вызовы разблокируются по id или при смерти процесса.
"""

from __future__ import annotations

import json
import logging
import os
import subprocess
import threading
import time
from typing import Any, Dict, Optional

logger = logging.getLogger(__name__)


class RpcError(RuntimeError):
    """Ошибка вызова (таймаут, JSON-RPC error)."""


class RpcDead(RpcError):
    """Процесс ob2h не запущен / умер."""


class Ob2hRpc:
    """Долгоживущее соединение с ``ob2h serve`` через subprocess stdio."""

    def __init__(
        self,
        argv,
        env: Optional[Dict[str, str]] = None,
        call_timeout: float = 900.0,
        startup_timeout: float = 30.0,
        restart_backoff: float = 5.0,
    ) -> None:
        self.argv = list(argv)
        self.env = env or {}
        self.call_timeout = call_timeout
        self.startup_timeout = startup_timeout
        self.restart_backoff = restart_backoff
        self._proc: Optional[subprocess.Popen] = None
        self._writer_lock = threading.Lock()
        self._pending: Dict[int, Dict[str, Any]] = {}
        self._pending_lock = threading.Lock()
        self._next_id = 0
        self._last_start = 0.0

    # -- жизненный цикл ------------------------------------------------------

    def start(self) -> None:
        """Запустить процесс и выполнить MCP-handshake. Идемпотентно."""
        with self._writer_lock:
            if self._proc and self._proc.poll() is None:
                return
            now = time.monotonic()
            if now - self._last_start < self.restart_backoff:
                raise RpcDead(f"ob2h: перезапуск чаще раза в {self.restart_backoff}с (backoff)")
            self._last_start = now

            env_full = dict(os.environ)
            env_full.update(self.env)
            # Windows: без консольного окна при запуске из GUI-контекстов Hermes.
            creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
            self._proc = subprocess.Popen(
                self.argv,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                env=env_full,
                creationflags=creationflags,
            )

        self._start_reader()
        self._handshake()
        logger.info("ob2h: процесс запущен (%s)", " ".join(self.argv))

    def _start_reader(self) -> None:
        proc = self._proc

        def _read() -> None:
            try:
                assert proc.stdout is not None
                for line in proc.stdout:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        msg = json.loads(line)
                    except ValueError:
                        continue
                    mid = msg.get("id")
                    if mid is None:
                        continue  # уведомления от сервера не ожидаем
                    with self._pending_lock:
                        slot = self._pending.get(mid)
                    if slot is not None:
                        slot["msg"] = msg
                        slot["event"].set()
            except Exception:
                pass
            # Процесс умер — разблокировать всех ждущих (без результата).
            with self._pending_lock:
                for slot in self._pending.values():
                    slot["event"].set()

        threading.Thread(target=_read, daemon=True, name="ob2h-rpc-reader").start()

    def _handshake(self) -> None:
        self.call(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "hermes-ob2h-plugin", "version": "0.8.0"},
            },
            timeout=self.startup_timeout,
        )
        try:
            self.notify("notifications/initialized")
        except RpcError:
            pass  # сервер относит это к уведомлениям молча

    def alive(self) -> bool:
        return bool(self._proc) and self._proc.poll() is None

    def ensure(self) -> None:
        """Поднять процесс, если он не жив (рестарт с backoff внутри start)."""
        if not self.alive():
            self.start()

    def stop(self) -> None:
        with self._writer_lock:
            proc, self._proc = self._proc, None
        if proc and proc.poll() is None:
            try:
                if proc.stdin:
                    proc.stdin.close()
            except Exception:
                pass
            # stdin закрыт = EOF: stdio-сервер завершается сам; даём ему
            # корректно закрыть SQLite (Windows держит файл БД до выхода).
            try:
                proc.wait(timeout=5.0)
                return
            except Exception:
                pass
            try:
                proc.terminate()
                proc.wait(timeout=3.0)
            except Exception:
                pass

    # -- протокол ------------------------------------------------------------

    @staticmethod
    def _write_line(proc: subprocess.Popen, payload: Dict[str, Any]) -> None:
        assert proc.stdin is not None
        try:
            proc.stdin.write((json.dumps(payload) + "\n").encode("utf-8"))
            proc.stdin.flush()
        except (BrokenPipeError, OSError) as e:
            raise RpcDead(f"ob2h: процесс мёртв (запись в stdin): {e}") from e

    def notify(self, method: str) -> None:
        payload = {"jsonrpc": "2.0", "method": method}
        with self._writer_lock:
            proc = self._proc
            if not proc or proc.poll() is not None:
                raise RpcDead("ob2h: процесс не запущен")
            self._write_line(proc, payload)

    def call(self, method: str, params: Optional[Dict[str, Any]] = None,
             timeout: Optional[float] = None) -> Any:
        timeout = timeout if timeout is not None else self.call_timeout
        with self._pending_lock:
            self._next_id += 1
            mid = self._next_id
            slot: Dict[str, Any] = {"event": threading.Event(), "msg": None}
            self._pending[mid] = slot
        try:
            payload: Dict[str, Any] = {"jsonrpc": "2.0", "id": mid, "method": method}
            if params is not None:
                payload["params"] = params
            with self._writer_lock:
                proc = self._proc
                if not proc or proc.poll() is not None:
                    raise RpcDead("ob2h: процесс не запущен")
                self._write_line(proc, payload)

            if not slot["event"].wait(timeout):
                raise RpcError(f"ob2h: таймаут {method} ({timeout}с)")
            msg = slot["msg"]
            if msg is None:
                raise RpcDead(f"ob2h: процесс умер во время {method}")
            error = msg.get("error")
            if error:
                raise RpcError(f"ob2h: {method}: {error.get('message')}")
            return msg.get("result")
        finally:
            with self._pending_lock:
                self._pending.pop(mid, None)

    # -- удобства ------------------------------------------------------------

    def ping(self, timeout: float = 5.0) -> bool:
        try:
            self.call("ping", {}, timeout=timeout)
            return True
        except RpcError:
            return False

    def tools_list(self) -> list:
        result = self.call("tools/list", {})
        return (result or {}).get("tools", [])

    def tool_call(self, name: str, args: Optional[Dict[str, Any]] = None,
                  timeout: Optional[float] = None) -> str:
        result = self.call("tools/call", {"name": name, "arguments": args or {}},
                           timeout=timeout)
        content = (result or {}).get("content") or []
        texts = [c.get("text", "") for c in content if isinstance(c, dict)]
        return "\n".join(t for t in texts if t)
