"""AutoDreamWorker: фоновый поток дрима с гейтами (порт AutoDreamWorker OmnesBOT).

Гейты (все должны пройти): ≥N часов с прошлого запуска, ≥M новых событий
daily-лога, lock-файл не занят (stale 1ч). Проверка каждые interval минут.
Плюс ретеншн daily-логов (retention_days).
"""

from __future__ import annotations

import json
import logging
import threading
import time
from collections.abc import Callable
from datetime import UTC, datetime, timedelta
from pathlib import Path

from .config import Settings
from .dream import Dream
from .workspace import Workspace

log = logging.getLogger("omnes.autodream")

LOCK_STALE_SEC = 3600


class AutoDreamWorker(threading.Thread):
    def __init__(
        self,
        dream: Dream,
        workspace: Workspace,
        settings: Settings,
        clock: Callable[[], datetime] | None = None,
        maintenance: Callable[[], None] | None = None,
    ):
        super().__init__(daemon=True, name="omnes-autodream")
        self.dream = dream
        self.workspace = workspace
        self.settings = settings
        self.maintenance = maintenance  # напр. decay/purge памяти (фаза 6.3)
        self._clock = clock or (lambda: datetime.now(UTC))
        self._stop = threading.Event()

    # --- жизненный цикл ---

    def run(self) -> None:  # threading.Thread.run
        interval = max(60, self.settings.autodream_interval_min * 60)
        while not self._stop.wait(interval):
            ok, reason = self.should_run()
            if not ok:
                log.debug("автодрим пропущен: %s", reason)
                continue
            if not self._acquire_lock():
                continue
            try:
                result = self.dream.run(trigger="auto")
                log.info("автодрим: %s", result.get("status"))
                self._save_last_run()
                if self.maintenance is not None:
                    try:
                        self.maintenance()
                    except Exception:
                        log.exception("maintenance после дрима не удался")
                self.prune_daily()
            finally:
                self._release_lock()

    def stop(self) -> None:
        self._stop.set()

    # --- гейты ---

    @property
    def state_file(self) -> Path:
        return self.settings.data_dir / "autodream_last_run.json"

    def last_run_iso(self) -> str | None:
        try:
            data = json.loads(self.state_file.read_text(encoding="utf-8"))
            return data.get("last_run")
        except (OSError, ValueError):
            return None

    def _save_last_run(self) -> None:
        self.settings.data_dir.mkdir(parents=True, exist_ok=True)
        self.state_file.write_text(
            json.dumps({"last_run": self._clock().isoformat()}, ensure_ascii=False),
            encoding="utf-8",
        )

    def should_run(self) -> tuple[bool, str]:
        last = self.last_run_iso()
        if last:
            last_dt = datetime.fromisoformat(last)
            elapsed = (self._clock() - last_dt).total_seconds() / 3600
            if elapsed < self.settings.autodream_min_interval_h:
                return False, f"прошло {elapsed:.1f}ч < {self.settings.autodream_min_interval_h}ч"
        fresh = self.workspace.count_daily_events_since(last or "")
        if fresh < self.settings.autodream_min_events:
            return False, f"новых событий {fresh} < {self.settings.autodream_min_events}"
        return True, "ok"

    # --- lock ---

    @property
    def lock_file(self) -> Path:
        return self.settings.data_dir / "autodream.lock"

    def _acquire_lock(self) -> bool:
        if self.lock_file.exists():
            age = time.time() - self.lock_file.stat().st_mtime
            if age < LOCK_STALE_SEC:
                return False
            log.warning("lock stale (%.0fс) — перехватываю", age)
        self.settings.data_dir.mkdir(parents=True, exist_ok=True)
        self.lock_file.write_text(str(time.time()), encoding="utf-8")
        return True

    def _release_lock(self) -> None:
        self.lock_file.unlink(missing_ok=True)

    # --- ретеншн ---

    def prune_daily(self) -> int:
        """Удаление daily-логов старше retention_days. Возвращает число файлов."""
        cutoff = self._clock() - timedelta(days=self.settings.retention_days)
        removed = 0
        for path in sorted((self.workspace.root / "daily").glob("*.jsonl")):
            try:
                day = datetime.strptime(path.stem, "%Y-%m-%d").replace(tzinfo=UTC)
            except ValueError:
                continue
            if day < cutoff:
                path.unlink()
                removed += 1
        return removed
