"""Git-история изменений MD-файлов workspace (порт GitStore из OmnesBOT).

Отдельный репозиторий внутри data/workspace/.git — не связан с git проекта.
Если git недоступен — все методы деградируют в no-op с предупреждением в лог:
сервер обязан работать и без git.
"""

from __future__ import annotations

import logging
import subprocess
from pathlib import Path

log = logging.getLogger("omnes.gitstore")

TRACKED = ["SOUL.md", "USER.md", "memory/MEMORY.md"]


class GitStore:
    def __init__(self, workspace_root: Path):
        self.root = Path(workspace_root)
        self._available: bool | None = None

    def _git(self, *args: str, check: bool = True) -> subprocess.CompletedProcess | None:
        try:
            proc = subprocess.run(
                ["git", "-C", str(self.root), *args],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=60,
            )
        except (OSError, subprocess.TimeoutExpired) as e:
            log.warning("git недоступен (%s); git-функции отключены", e)
            self._available = False
            return None
        if check and proc.returncode != 0:
            log.warning("git %s: %s", args[0], proc.stderr.strip()[:200])
            return None
        return proc

    def ensure_repo(self) -> bool:
        if self._available is not None:
            return self._available
        if (self.root / ".git").exists():
            self._available = True
            return True
        proc = self._git("init", "-q", "-b", "main")
        if proc is None:
            self._available = False
            return False
        # Локальная идентичность коммитов (не трогаем глобальный git-конфиг)
        self._git("config", "user.email", "omnes-dream@local")
        self._git("config", "user.name", "OmnesMemory Dream")
        self._available = True
        return True

    def auto_commit(self, message: str) -> str | None:
        """Коммит отслеживаемых файлов; возвращает sha или None (нет изменений/git)."""
        if not self.ensure_repo():
            return None
        files = [f for f in TRACKED if (self.root / f).exists()]
        if not files:
            return None
        self._git("add", "--", *files)
        status = self._git("status", "--porcelain", "--", *files)
        if status is None or not status.stdout.strip():
            return None
        if self._git("commit", "-q", "-m", message) is None:
            return None
        sha = self._git("rev-parse", "--short", "HEAD")
        return sha.stdout.strip() if sha else None

    def log(self, limit: int = 20) -> list[dict[str, str]]:
        """История коммитов: [{sha, date, message}]."""
        if not self.ensure_repo():
            return []
        proc = self._git(
            "log", "-n", str(limit), "--format=%h%x09%ad%x09%s",
            "--date=format:%Y-%m-%d %H:%M",
        )
        if proc is None:
            return []
        entries = []
        for line in proc.stdout.splitlines():
            parts = line.split("\t", 2)
            if len(parts) == 3:
                sha, date, message = parts
                entries.append({"sha": sha, "date": date, "message": message})
        return entries

    def restore(self, ref: str) -> str:
        """Откат отслеживаемых файлов к состоянию коммита ref (sha или 'HEAD~n')."""
        if not self.ensure_repo():
            return "git_unavailable"
        files = [f for f in TRACKED if (self.root / f).exists()]
        if not files:
            return "nothing_to_restore"
        proc = self._git("checkout", ref, "--", *files)
        if proc is None:
            return f"restore_failed: {ref}"
        return f"restored from {ref}"
