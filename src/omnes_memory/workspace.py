"""Файловый workspace агента: MEMORY/SOUL/USER.md, history.jsonl, курсоры.

Порт MemoryStore из app/core/omnesbot/agent/memory.py (OmnesBOT), упрощён
до одного локального пользователя. Курсоры консолидатора (.cursor) и дрима
(.dream_cursor) — Files-файлы для возобновляемости после любого сбоя.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path

FILES = {
    "memory": "memory/MEMORY.md",
    "soul": "SOUL.md",
    "user": "USER.md",
}

MAX_HISTORY = 1000  # compact_history() из OmnesBOT

DEFAULTS = {
    "memory/MEMORY.md": "# Долгосрочная память\n\n- (пусто — дрим и агент наполнят)\n",
    "SOUL.md": "# SOUL\n\nИдентичность агента. Заполняется владельцем и дримом.\n",
    "USER.md": "# USER\n\nФакты о владельце. Заполняется агентом и дримом.\n",
}


class Workspace:
    def __init__(self, root: Path):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)
        (self.root / "memory").mkdir(exist_ok=True)
        (self.root / "daily").mkdir(exist_ok=True)
        self._ensure_defaults()

    def _ensure_defaults(self) -> None:
        for rel, default in DEFAULTS.items():
            path = self.root / rel
            if not path.exists():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(default, encoding="utf-8")

    # --- MD-файлы ---

    def path_of(self, name: str) -> Path:
        rel = FILES.get(name, name if name in ("history",) else None)
        if rel is None:
            raise ValueError(f"Неизвестный файл workspace: {name!r} (memory|soul|user|history)")
        return self.root / "memory/history.jsonl" if name == "history" else self.root / rel

    def read(self, name: str) -> str:
        if name == "history":
            return "\n".join(json.dumps(r, ensure_ascii=False) for r in self.load_history())
        return self.path_of(name).read_text(encoding="utf-8")

    def write(self, name: str, content: str) -> str:
        if name == "history":
            raise ValueError("history.jsonl пишется только через append_history/compact")
        path = self.path_of(name)
        path.write_text(content, encoding="utf-8")
        return str(path.relative_to(self.root))

    # --- history.jsonl: {"cursor": int, "timestamp": "...", "content": str} ---

    @property
    def history_path(self) -> Path:
        return self.root / "memory" / "history.jsonl"

    def load_history(self) -> list[dict]:
        if not self.history_path.exists():
            return []
        records = []
        for line in self.history_path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line:
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError:
                    continue  # битая строка не роняет загрузку
        return records

    def append_history(self, content: str, cursor: int | None = None) -> dict:
        records = self.load_history()
        last_cursor = max((r.get("cursor", 0) for r in records), default=0)
        record = {
            "cursor": cursor if cursor is not None else last_cursor + 1,
            "timestamp": datetime.now(UTC).strftime("%Y-%m-%d %H:%M"),
            "content": content,
        }
        with self.history_path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
        return record

    def compact_history(self, max_items: int = MAX_HISTORY) -> int:
        """Оставляет последние max_items записей (порт compact_history)."""
        records = self.load_history()
        if len(records) <= max_items:
            return 0
        keep = records[-max_items:]
        tmp = self.history_path.with_suffix(".jsonl.tmp")
        with tmp.open("w", encoding="utf-8") as f:
            for r in keep:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        tmp.replace(self.history_path)
        return len(records) - len(keep)

    # --- курсоры ---

    def get_cursor(self, name: str) -> int:
        path = self.root / "memory" / f".{name}"
        if not path.exists():
            return 0
        try:
            return int(path.read_text(encoding="utf-8").strip() or "0")
        except ValueError:
            return 0

    def set_cursor(self, name: str, value: int) -> None:
        (self.root / "memory" / f".{name}").write_text(str(value), encoding="utf-8")

    # --- daily-логи (вход для автодрима и ретеншна, схема MemoryV2) ---

    def daily_file(self, day: str | None = None) -> Path:
        day = day or datetime.now(UTC).strftime("%Y-%m-%d")
        return self.root / "daily" / f"{day}.jsonl"

    def append_daily_event(self, event: dict) -> None:
        with self.daily_file().open("a", encoding="utf-8") as f:
            f.write(json.dumps(event, ensure_ascii=False) + "\n")

    def load_daily_events(self, day: str | None = None) -> list[dict]:
        path = self.daily_file(day)
        if not path.exists():
            return []
        events = []
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line:
                try:
                    events.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
        return events

    def count_daily_events_since(self, iso_ts: str) -> int:
        """Сколько событий daily-логов новее метки времени (для гейта автодрима)."""
        total = 0
        daily_dir = self.root / "daily"
        for path in sorted(daily_dir.glob("*.jsonl")):
            for event in self.load_daily_events(path.stem):
                if str(event.get("timestamp", "")) > iso_ts:
                    total += 1
        return total
