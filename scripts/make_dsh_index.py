"""Генератор маркет-индекса DSH: ``index.json`` в корне репозитория.

Формат — plugin market index, format 1 (DeepSeek Harness). Индекс читается DSH
из Git-репозитория (``index.json`` в корне на ветке по умолчанию); каждая версия
пакета закреплена за коммитом, поэтому порядок публикации такой:

    1. коммит с содержимым пакета ``plugin/dsh-ob2h``;
    2. ``python scripts/make_dsh_index.py <версия> <40-hex SHA этого коммита>``;
    3. коммит с ``index.json``.

Использование:
    python scripts/make_dsh_index.py <версия> <коммит>   # добавить/обновить версию, сделать её latest
    python scripts/make_dsh_index.py --check <версия>    # только сверить индекс с пакетом

Только stdlib. Иконка — data URL не больше 32 КиБ (для Git-источников индекса
относительные ссылки не работают).
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKAGE_NAME = "dsh-ob2h"
PACKAGE_DIR = "plugin/dsh-ob2h"
ICON = ROOT / PACKAGE_DIR / "icon-128.png"
INDEX = ROOT / "index.json"
REPO_URL = "https://github.com/Pofium/ob2h"
MAX_INLINE_ICON = 32 * 1024
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")

MARKET = {
    "format": 1,
    "title": {"en": "OB2H plugins", "zh": "OB2H 插件", "ru": "Плагины OB2H"},
    "description": {
        "en": "Plugins that connect DeepSeek Harness to OB2H, a local store for long-term memory, a knowledge graph and an AST graph of your code.",
        "zh": "将 DeepSeek Harness 连接到 OB2H 的插件：OB2H 是一个本地存储，提供长期记忆、知识图谱和代码的 AST 图谱。",
        "ru": "Плагины, подключающие DeepSeek Harness к OB2H — локальному хранилищу долговременной памяти, графа знаний и AST-графа кода.",
    },
    "metadata": {
        "categories": {
            "memory": {"title": {"en": "Memory", "zh": "记忆", "ru": "Память"}},
        },
    },
}

# Короткие тексты карточки индекса; длинное описание DSH берёт из локалей пакета.
PLUGIN = {
    "name": PACKAGE_NAME,
    "title": {"en": "OB2H memory", "zh": "OB2H 记忆", "ru": "Память OB2H"},
    "description": {
        "en": "Long-term memory for the agent: the 57 ob2h tools, recall of relevant memories before each turn and saving of finished turns. Configure both rows before enabling them.",
        "zh": "为智能体提供长期记忆：57 个 ob2h 工具，每一轮对话前回忆相关记忆，结束后保存对话。启用前请先配置两个插件行。",
        "ru": "Долговременная память агента: 57 инструментов ob2h, автоматический recall перед каждым ходом и сохранение завершённых ходов. Перед включением настройте обе строки.",
    },
    "author": "Pofium",
    "homepage": REPO_URL,
    "repository": {"url": REPO_URL, "directory": PACKAGE_DIR},
    "metadata": {
        "category": "memory",
        "keywords": [
            "ob2h",
            "memory",
            "mcp",
            "knowledge-graph",
            "code-graph",
            "deepseek-harness",
        ],
    },
}


def git(*args: str) -> bytes:
    """`git` в корне репозитория; ошибка git — ошибка генератора."""
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, check=True).stdout


def package_at(commit: str) -> dict:
    """Манифест пакета ровно в том виде, в каком он лежит в коммите."""
    return json.loads(git("show", f"{commit}:{PACKAGE_DIR}/package.json"))


def icon_data_url() -> str:
    payload = ICON.read_bytes()
    if payload[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"Не PNG: {ICON}")
    if len(payload) > MAX_INLINE_ICON:
        sys.exit(f"Иконка {len(payload)} байт больше лимита {MAX_INLINE_ICON}")
    return "data:image/png;base64," + base64.b64encode(payload).decode("ascii")


def other_versions(version: str) -> list[dict]:
    """Версии из существующего индекса, кроме добавляемой."""
    if not INDEX.is_file():
        return []
    index = json.loads(INDEX.read_text(encoding="utf-8"))
    for entry in index.get("plugins", []):
        if entry.get("name") == PACKAGE_NAME:
            return [v for v in entry.get("versions", []) if v.get("version") != version]
    return []


def build(version: str, commit: str) -> dict:
    manifest = package_at(commit)
    if manifest.get("name") != PACKAGE_NAME:
        sys.exit(f"В коммите {commit[:12]} пакет называется {manifest.get('name')!r}")
    if manifest.get("version") != version:
        sys.exit(f"version в коммите {manifest['version']} != {version}")
    record = {
        "version": version,
        "publishTimestamp": int(git("show", "-s", "--format=%ct", commit).decode().strip()),
        "source": {"type": "git", "url": REPO_URL, "commit": commit, "path": PACKAGE_DIR},
        "engines": {"dsh": manifest["engines"]["dsh"]},
    }
    plugin = json.loads(json.dumps(PLUGIN))
    plugin["icon"] = icon_data_url()
    plugin["dist-tags"] = {"latest": version}
    plugin["versions"] = [record, *other_versions(version)]
    return {**json.loads(json.dumps(MARKET)), "plugins": [plugin]}


def check(version: str) -> list[str]:
    """Расхождения записи версии с манифестом пакета в закреплённом коммите."""
    index = json.loads(INDEX.read_text(encoding="utf-8")) if INDEX.is_file() else {}
    plugin = next((p for p in index.get("plugins", []) if p.get("name") == PACKAGE_NAME), None)
    if plugin is None:
        return [f"плагин {PACKAGE_NAME} отсутствует в индексе"]
    record = next((v for v in plugin.get("versions", []) if v.get("version") == version), None)
    if record is None:
        return [f"версия {version} отсутствует в индексе"]
    manifest = package_at(record["source"]["commit"])
    problems = []
    if manifest.get("version") != version:
        problems.append(f"version в коммите {manifest.get('version')} != {version}")
    if record.get("engines", {}).get("dsh") != manifest.get("engines", {}).get("dsh"):
        problems.append("engines.dsh в индексе отличается от манифеста в коммите")
    if record["source"].get("path") != PACKAGE_DIR:
        problems.append("source.path не указывает на каталог пакета")
    if plugin.get("icon") != icon_data_url():
        problems.append("иконка индекса отличается от icon-128.png")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description="Генератор маркет-индекса DSH (index.json).")
    parser.add_argument("version", help="версия пакета (совпадает с package.json в коммите)")
    parser.add_argument("commit", nargs="?", help="полный SHA коммита с содержимым пакета")
    parser.add_argument("--check", action="store_true", help="только сверить индекс с пакетом")
    args = parser.parse_args()
    if not VERSION_RE.match(args.version):
        sys.exit(f"Некорректная версия: {args.version}")
    if args.check:
        problems = check(args.version)
        for line in problems:
            print("  -", line)
        print("OK: индекс соответствует пакету" if not problems else f"FAIL: {len(problems)}")
        return 1 if problems else 0
    if not args.commit or not COMMIT_RE.match(args.commit):
        sys.exit("Укажите полный SHA (40 hex) коммита с содержимым пакета или --check")
    INDEX.write_text(
        json.dumps(build(args.version, args.commit), ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"Записан index.json: {PACKAGE_NAME} {args.version} <- {args.commit[:12]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
