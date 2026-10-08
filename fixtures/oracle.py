#!/usr/bin/env python3
"""Run a v1 parser over one fixture and print its turns as JSON.

The v1 servers are the parity oracle for the Rust parsers. This harness reads their
source, stubs out the third-party imports they pull in at module scope, and
calls `_parse_conversation`. It never writes to, imports side effects into, or
otherwise touches the v1 scripts or the real transcript roots.

    uv run --no-project python fixtures/oracle.py claude_code <path.jsonl>
    uv run --no-project python fixtures/oracle.py codex <path.jsonl>
"""
import json
import pathlib
import sys
import types

V1_SOURCES = {
    "claude_code": pathlib.Path("~/.claude/mcp/conversation_search.py").expanduser(),
    "codex": pathlib.Path("~/.codex/mcp/codex_conversation_search.py").expanduser(),
}


def install_import_stubs() -> None:
    """Satisfy the v1 module-level imports without installing their packages."""
    bm25s = types.ModuleType("bm25s")
    bm25s.BM25 = type("BM25", (), {})
    bm25s.tokenize = lambda *a, **k: None
    sys.modules["bm25s"] = bm25s

    mcp = types.ModuleType("mcp")
    server = types.ModuleType("mcp.server")
    fastmcp = types.ModuleType("mcp.server.fastmcp")

    class FastMCP:
        def __init__(self, *a, **k):
            pass

        def tool(self, *a, **k):
            return lambda fn: fn

        def run(self, *a, **k):
            raise RuntimeError("the oracle never runs the v1 server")

    fastmcp.FastMCP = FastMCP
    server.fastmcp = fastmcp
    mcp.server = server
    sys.modules["mcp"] = mcp
    sys.modules["mcp.server"] = server
    sys.modules["mcp.server.fastmcp"] = fastmcp

    watchdog = types.ModuleType("watchdog")
    events = types.ModuleType("watchdog.events")
    events.FileSystemEventHandler = type("FileSystemEventHandler", (), {})
    observers = types.ModuleType("watchdog.observers")
    observers.Observer = type("Observer", (), {})
    watchdog.events = events
    watchdog.observers = observers
    sys.modules["watchdog"] = watchdog
    sys.modules["watchdog.events"] = events
    sys.modules["watchdog.observers"] = observers


def load_v1_parser(cli: str):
    source_path = V1_SOURCES[cli]
    module = types.ModuleType(f"v1_{cli}")
    module.__file__ = str(source_path)
    code = compile(source_path.read_text(encoding="utf-8"), str(source_path), "exec")
    exec(code, module.__dict__)
    return module._parse_conversation


def main() -> int:
    if len(sys.argv) != 3 or sys.argv[1] not in V1_SOURCES:
        print(__doc__, file=sys.stderr)
        return 2

    cli, target = sys.argv[1], pathlib.Path(sys.argv[2])
    install_import_stubs()
    parse = load_v1_parser(cli)
    turns, metadata = parse(target)

    json.dump(
        {
            "turn_count": len(turns),
            "project": metadata.get("project", ""),
            "session_id": metadata.get("session_id", ""),
            "slug": metadata.get("slug", ""),
            "first_timestamp": metadata.get("first_timestamp", ""),
            "last_timestamp": metadata.get("last_timestamp", ""),
            "turns": [
                {
                    "turn_number": turn["turn_number"],
                    "timestamp": turn["timestamp"],
                    "text": turn["text"],
                }
                for turn in turns
            ],
        },
        sys.stdout,
        ensure_ascii=False,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
