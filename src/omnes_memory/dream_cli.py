"""CLI для дрима и бэкапа без Hermes (см. docs/HERMES_INTEGRATION.md §4).

Примеры:
    python -m omnes_memory.dream_cli run
    python -m omnes_memory.dream_cli status
    python -m omnes_memory.dream_cli backup
"""

from __future__ import annotations

import argparse
import json
import sys

from .backup import Backup
from .config import get_settings
from .server import setup_logging


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="omnes-dream")
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("run", help="запустить дрим вручную")
    sub.add_parser("status", help="статус последнего дрима")
    sub.add_parser("backup", help="создать бэкап")
    args = parser.parse_args(argv)

    settings = get_settings()
    setup_logging(settings)

    from .db import Database
    from .dream import Dream
    from .gitstore import GitStore
    from .llm_client import make_llm
    from .workspace import Workspace

    settings.ensure_dirs()
    db = Database(settings.db_path)
    try:
        if args.cmd == "backup":
            print(Backup(settings).create())
            return 0
        if args.cmd == "status":
            row = db.query_one(
                "SELECT id, started_at, finished_at, status, trigger, stats "
                "FROM dream_runs ORDER BY id DESC LIMIT 1"
            )
            print(json.dumps(dict(row), ensure_ascii=False, indent=2) if row
                  else "дримов ещё не было")
            return 0
        # run
        workspace = Workspace(settings.workspace_dir)
        gitstore = GitStore(settings.workspace_dir)
        llm = make_llm(settings)
        if llm is None:
            print("[Error] OMNES_LLM_API_KEY не задан", file=sys.stderr)
            return 1
        result = Dream(workspace, gitstore, llm, settings, db).run(trigger="cli")
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if result["status"] == "ok" else 1
    finally:
        db.close()


if __name__ == "__main__":
    sys.exit(main())
