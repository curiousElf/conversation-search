# conversation-search

An MCP server that lets Claude Code and Codex search their own past conversations.

A background daemon watches the transcript folders of both CLIs, indexes every turn with BM25 (tantivy, with metadata in SQLite), and serves the index over MCP on `http://127.0.0.1:7317/mcp`. New turns become searchable about five seconds after they are written.

## Why I built this
I run quite a few sessions that tend to depend on previous work - the harnesses themselves are not great at picking out exact items that I wanted. The original MCP idea by Eric worked well until it didn't. With subagents and parallel sessions, the MCP would freeze intermittently and leave orphan sessions. Eventually it turned into a resource hog that required me to kill orphan processes periodically.
I thought it would be better instead if I had a daemon process running that was indexing my transcripts. I thought I might as well try a systems language, as this seems a sweet spot for the likes of Go, Rust etc. After some back and forth with the AI, I settled on Rust w/ Tantivy, as I had been wanting to build something in it for a while.

## Credit

This project builds on Eric W. Tramel's post [Searchable Agent Memory in a Single File](https://eric-tramel.github.io/blog/2026-02-07-searchable-agent-memory/). The post describes a single-file Python MCP server that indexes Claude Code JSONL transcripts with BM25. This repo rewrites that idea in Rust as one long-running daemon that also covers Codex.

## Tools

| Tool | What it does |
|---|---|
| `search_conversations` | BM25 keyword search across conversation turns |
| `list_conversations` | Browse indexed conversations, newest first |
| `read_turn` | Full text of one turn |
| `read_conversation` | Page through the turns of one conversation |
| `index_status` | Index counts, generation, and daemon health |

Every tool except `index_status` takes a required `cli` argument: `claude_code`, `codex`, or `all`.

## What gets indexed

- Claude Code: `~/.claude/projects/**/*.jsonl`, including subagent transcripts.
- Codex: `~/.codex/sessions/**/*.jsonl`.

Transcripts are only ever read. Codex turns pass through secret redaction for token, password and API-key assignments and MongoDB connection strings before indexing.

## Install

Requires macOS (the daemon runs under launchd), a Rust toolchain, and both the `claude` and `codex` CLIs on `PATH`.

```sh
make install
```

This builds a release binary to `~/.local/bin/conversation-search`, starts it as a launchd agent, waits for the first index build, and registers it as `conversation-search-v2` with both CLIs. Sessions that were already open need a restart to see it.

| Path | Contents |
|---|---|
| `~/Library/Application Support/conversation-search` | Index stores |
| `~/Library/Logs/conversation-search` | Daemon logs |

```sh
make uninstall   # stop the daemon and unregister; keeps the index and logs
make purge       # delete the index and logs
```

## Command line

```sh
conversation-search status                       # health, counts, pending files
conversation-search search "docker cache" --cli all
conversation-search reindex                      # rebuild the index from scratch
conversation-search restart                      # restart the launchd agent
```

## Tests

```sh
make test
```

## License

MIT
