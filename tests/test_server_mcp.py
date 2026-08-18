"""Интеграционный тест MCP-сервера: спавн через stdio и вызов инструментов (фаза 2.6).

Сессия создаётся внутри тела теста (enter/exit в одной задаче): anyio запрещает
покидать cancel scope в другой задаче, поэтому фикстура-обёртка не подходит.
"""

import contextlib
import os
import sys

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


def server_params(tmp_path):
    env = {
        **os.environ,
        "OMNES_DATA_DIR": str(tmp_path / "data"),
        "OMNES_EMBED_PROVIDER": "fake",
        "OMNES_LOG_LEVEL": "ERROR",
        "PYTHONUNBUFFERED": "1",
    }
    return StdioServerParameters(
        command=sys.executable,
        args=["-m", "omnes_memory.server"],
        env=env,
    )


@contextlib.asynccontextmanager
async def mcp_server(tmp_path):
    async with (
        stdio_client(server_params(tmp_path)) as (read, write),
        ClientSession(read, write) as session,
    ):
        await session.initialize()
        yield session


async def test_tools_listed(tmp_path):
    async with mcp_server(tmp_path) as session:
        tools = await session.list_tools()
        names = {t.name for t in tools.tools}
        assert {"memory_save", "memory_search", "memory_context",
                "workspace_read", "omnes_stats"} <= names


async def test_save_then_search_scenario(tmp_path):
    async with mcp_server(tmp_path) as session:
        r1 = await session.call_tool("memory_save", {
            "content": "Дрейзен-котёл установлен в третьем цеху", "key": "kotol",
        })
        assert "created" in r1.content[0].text

        await session.call_tool("memory_save", {
            "content": "Любимое блюдо — борщ с чесноком", "key": "food",
        })

        # fts-режим детерминирован; в hybrid fts-хит тоже всегда первый (RRF),
        # но с fake-векторами проверку отсутствия второго делаем в fts-режиме
        found = await session.call_tool(
            "memory_search", {"query": "Дрейзен-котёл", "mode": "fts"}
        )
        text = found.content[0].text
        assert "kotol" in text and "food" not in text

        hybrid = await session.call_tool("memory_search", {"query": "Дрейзен-котёл"})
        assert "kotol" in hybrid.content[0].text.splitlines()[0]


async def test_workspace_write_and_read(tmp_path):
    async with mcp_server(tmp_path) as session:
        await session.call_tool("workspace_write", {
            "file": "memory", "content": "# Память\n- факт из теста\n",
        })
        r = await session.call_tool("workspace_read", {"file": "memory"})
        assert "факт из теста" in r.content[0].text


async def test_error_returns_string_not_exception(tmp_path):
    async with mcp_server(tmp_path) as session:
        r = await session.call_tool("workspace_read", {"file": "нет такого"})
        assert r.content[0].text.startswith("[Error]")


async def test_stats(tmp_path):
    async with mcp_server(tmp_path) as session:
        await session.call_tool("memory_save", {"content": "факт"})
        r = await session.call_tool("omnes_stats", {})
        assert "memories=1" in r.content[0].text
