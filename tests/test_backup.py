"""Тесты бэкапов: VACUUM INTO, копия workspace, ротация (фаза 6.2)."""

import sqlite3

import pytest

from omnes_memory.backup import Backup
from omnes_memory.config import Settings
from omnes_memory.db import Database
from omnes_memory.workspace import Workspace


@pytest.fixture
def settings(tmp_path):
    return Settings(_env_file=None, data_dir=tmp_path / "data")


def test_backup_creates_consistent_copy(settings):
    settings.ensure_dirs()
    db = Database(settings.db_path)
    db.execute(
        "INSERT INTO memories (key, content, created_at, updated_at) "
        "VALUES ('k1', 'факт', '2026', '2026')"
    )
    ws = Workspace(settings.workspace_dir)
    ws.write("memory", "# изменилось")
    db.close()

    backup = Backup(settings, keep=14)
    target = backup.create()

    assert (target / "omnes.db").exists()
    assert (target / "workspace" / "memory" / "MEMORY.md").read_text(
        encoding="utf-8") == "# изменилось"

    # снимок открывается и содержит данные
    snap = sqlite3.connect(target / "omnes.db")
    count = snap.execute("SELECT count(*) FROM memories").fetchone()[0]
    snap.close()
    assert count == 1


def test_backup_rotation_keeps_recent(settings):
    settings.ensure_dirs()
    backup = Backup(settings, keep=3)
    for i in range(5):
        d = settings.backups_dir / f"2026-01-0{i+1}_000000"
        d.mkdir(parents=True)
    removed = backup.rotate()
    assert removed == 2
    assert len(backup.list()) == 3
    assert backup.list()[0] == "2026-01-03_000000"  # старые удалены


def test_backup_skips_lock_file(settings):
    settings.ensure_dirs()
    (settings.workspace_dir / "autodream.lock").write_text("1", encoding="utf-8")
    target = Backup(settings).create()
    assert not (target / "workspace" / "autodream.lock").exists()


def test_backup_list_empty_dir(settings):
    settings.ensure_dirs()
    assert Backup(settings).list() == []
