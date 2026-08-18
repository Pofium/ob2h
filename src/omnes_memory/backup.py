"""Бэкапы: атомарная копия БД (VACUUM INTO) + workspace, ротация (фаза 6.2)."""

from __future__ import annotations

import logging
import shutil
import sqlite3
from datetime import datetime
from pathlib import Path

from .config import Settings

log = logging.getLogger("omnes.backup")

KEEP_DEFAULT = 14


class Backup:
    def __init__(self, settings: Settings, keep: int = KEEP_DEFAULT):
        self.settings = settings
        self.keep = keep

    def create(self) -> Path:
        stamp = datetime.now().strftime("%Y-%m-%d_%H%M%S")
        target = self.settings.backups_dir / stamp
        target.mkdir(parents=True, exist_ok=False)

        db_path = self.settings.db_path
        if db_path.exists():
            # VACUUM INTO даёт согласованный снимок без остановки сервера
            dest = target / "omnes.db"
            conn = sqlite3.connect(str(db_path))
            try:
                conn.execute("VACUUM INTO ?", (str(dest),))
            finally:
                conn.close()

        workspace = self.settings.workspace_dir
        if workspace.exists():
            shutil.copytree(workspace, target / "workspace",
                            dirs_exist_ok=True, ignore=shutil.ignore_patterns(
                                "autodream.lock"))

        self.rotate()
        log.info("бэкап создан: %s", target)
        return target

    def rotate(self) -> int:
        """Оставляет последние self.keep бэкапов. Возвращает число удалённых."""
        backups = sorted(
            (p for p in self.settings.backups_dir.iterdir() if p.is_dir()),
            key=lambda p: p.name,
        )
        removed = 0
        for old in backups[: max(0, len(backups) - self.keep)]:
            shutil.rmtree(old)
            removed += 1
        return removed

    def list(self) -> list[str]:
        if not self.settings.backups_dir.exists():
            return []
        return sorted(p.name for p in self.settings.backups_dir.iterdir() if p.is_dir())
