#!/usr/bin/env python3
"""Regenerate the synthetic transcript fixtures.

The record shapes copy what the two CLIs actually write; every string is
invented. Run from the repository root:

    uv run --no-project python fixtures/make_fixtures.py
"""
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent


def write(path: pathlib.Path, records: list) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for record in records:
            if isinstance(record, str):
                handle.write(record + "\n")
            else:
                handle.write(json.dumps(record, ensure_ascii=False) + "\n")
    print(f"wrote {path.relative_to(ROOT.parent)}")


def claude_common(uuid: str, ts: str) -> dict:
    return {
        "parentUuid": None,
        "isSidechain": False,
        "userType": "external",
        "cwd": "/Users/me/Projects/personal/widget-shop",
        "sessionId": "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b",
        "version": "2.1.252",
        "gitBranch": "main",
        "uuid": uuid,
        "timestamp": ts,
    }


def claude_main_session() -> list:
    long_command = "for f in $(ls -1 build/artifacts); do printf '%s\\n' \"$f\"; done | sort -u | head -40"
    return [
        {"type": "mode", "mode": "default", "sessionId": "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b"},
        {"type": "permission-mode", "permissionMode": "acceptEdits",
         "sessionId": "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b"},
        {"type": "ai-title", "aiTitle": "Widget shop build",
         "sessionId": "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b"},
        dict(claude_common("aaaa0001-0000-4000-8000-000000000001", "2026-02-03T09:00:00.000Z"),
             type="user", slug="widget-shop-build", entrypoint="cli", promptSource="user",
             promptId="p1", origin="terminal", permissionMode="acceptEdits",
             message={"role": "user", "content": "Why does the widget build drop the sprite sheet?"}),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000002", "2026-02-03T09:00:04.100Z"),
             type="assistant", requestId="req_001", effort="medium",
             message={
                 "id": "msg_001", "type": "message", "role": "assistant", "model": "claude-fable-5",
                 "stop_reason": None, "stop_sequence": None, "stop_details": None,
                 "diagnostics": [], "usage": {"input_tokens": 12, "output_tokens": 30},
                 "content": [
                     {"type": "thinking", "thinking": "internal notes that must not be indexed",
                      "signature": "sig-abc"},
                     {"type": "text", "text": "The sprite sheet is written after the copy step."},
                     {"type": "tool_use", "id": "toolu_01", "name": "Bash",
                      "input": {"command": long_command, "description": "list artifacts"}},
                     {"type": "tool_use", "id": "toolu_02", "name": "Read",
                      "input": {"file_path": "/Users/me/Projects/personal/widget-shop/build.mjs"}},
                 ],
             }),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000003", "2026-02-03T09:00:06.000Z"),
             type="user", promptId="p1", sourceToolAssistantUUID="aaaa0001-0000-4000-8000-000000000002",
             toolUseResult={"stdout": "sprites.png\n", "stderr": "", "interrupted": False},
             message={"role": "user", "content": [
                 {"type": "tool_result", "tool_use_id": "toolu_01", "content": "sprites.png\n"}]}),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000004", "2026-02-03T09:00:07.000Z"),
             type="user", isMeta=True, sourceToolUseID="toolu_02", promptId="p1",
             message={"role": "user", "content": [
                 {"type": "text", "text": "Caveat: this session is a continuation."}]}),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000005", "2026-02-03T09:00:08.000Z"),
             type="user", promptId="p2",
             message={"role": "user", "content": "<command-name>/compact</command-name>"}),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000006", "2026-02-03T09:00:09.000Z"),
             type="user", promptId="p2",
             message={"role": "user",
                      "content": "<local-command-stdout>compacted 40 messages</local-command-stdout>"}),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000007", "2026-02-03T09:00:11.000Z"),
             type="assistant", requestId="req_002",
             message={"id": "msg_002", "type": "message", "role": "assistant",
                      "model": "claude-fable-5", "content": [
                          {"type": "text", "text": "Reordering the copy step fixes it."}]}),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000008", "2026-02-03T09:05:00.000Z"),
             type="user", promptId="p3",
             message={"role": "user",
                      "content": "Now check the gadget renderer — the café ☕ glyph renders twice."}),
        dict(claude_common("aaaa0001-0000-4000-8000-000000000009", "2026-02-03T09:05:03.000Z"),
             type="assistant", requestId="req_003", attributionSkill="frontend-design",
             message={"id": "msg_003", "type": "message", "role": "assistant",
                      "model": "claude-fable-5", "content": [
                          {"type": "text", "text": "The glyph is emitted by both the shaper and the fallback."},
                          {"type": "tool_use", "id": "toolu_03", "name": "Grep",
                           "input": {"pattern": "renderGlyph", "path": "src"}},
                          {"type": "tool_use", "id": "toolu_04", "name": "Task",
                           "input": {"description": "audit glyph fallbacks", "subagent_type": "Explore"}},
                          {"type": "tool_use", "id": "toolu_05", "name": "WebFetch",
                           "input": {"url": "https://example.invalid/harfbuzz-notes"}},
                          {"type": "tool_use", "id": "toolu_06", "name": "Bash", "input": {}},
                          {"type": "tool_use", "id": "toolu_07", "name": "NotebookEdit",
                           "input": {"notebook_path": "x.ipynb"}},
                      ]}),
        {"type": "system", "subtype": "hook", "level": "info", "isSidechain": False,
         "sessionId": "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b", "timestamp": "2026-02-03T09:05:04.000Z",
         "uuid": "aaaa0001-0000-4000-8000-00000000000a", "cwd": "/Users/me/Projects/personal/widget-shop",
         "content": "hook ran", "isMeta": True},
        "this line is not json and must be skipped",
        "",
        dict(claude_common("aaaa0001-0000-4000-8000-00000000000b", "2026-02-03T09:09:00.000Z"),
             type="user", promptId="p4",
             message={"role": "user", "content": "Third question: does the loader cache survive a restart?"}),
        dict(claude_common("aaaa0001-0000-4000-8000-00000000000c", "2026-02-03T09:09:02.000Z"),
             type="assistant", requestId="req_004",
             message={"id": "msg_004", "type": "message", "role": "assistant",
                      "model": "claude-fable-5", "content": [
                          {"type": "text", "text": "It does not; the cache is process-local."},
                          {"type": "tool_use", "id": "toolu_08", "name": "Write",
                           "input": {"file_path": "/Users/me/Projects/personal/widget-shop/loader.mjs"}},
                      ]}),
    ]


def claude_subagent_session() -> list:
    base = {
        "parentUuid": None, "isSidechain": True, "userType": "external",
        "cwd": "/Users/me/Projects/personal/widget-shop",
        "sessionId": "9c8b7a69-5040-4312-9182-736455647382",
        "version": "2.1.252", "gitBranch": "main",
    }
    return [
        dict(base, type="user", uuid="bbbb0001-0000-4000-8000-000000000001",
             timestamp="2026-02-03T09:05:05.000Z", slug="glyph-audit",
             message={"role": "user", "content": "Audit every glyph fallback in the renderer."}),
        dict(base, type="assistant", uuid="bbbb0001-0000-4000-8000-000000000002",
             timestamp="2026-02-03T09:05:20.000Z", requestId="req_sub_1",
             message={"id": "msg_sub_1", "type": "message", "role": "assistant",
                      "model": "claude-fable-5", "content": [
                          {"type": "text", "text": "Two fallbacks overlap in the shaper."},
                          {"type": "tool_use", "id": "toolu_s1", "name": "Glob",
                           "input": {"pattern": "src/**/*.mjs"}},
                      ]}),
        # Empty user content: v1 opens a turn that can never be saved. Kept so the
        # port has to reproduce that behaviour.
        dict(base, type="user", uuid="bbbb0001-0000-4000-8000-000000000003",
             timestamp="2026-02-03T09:05:30.000Z",
             message={"role": "user", "content": ""}),
    ]


def codex_session() -> list:
    long_output = "warning: unused import\n" * 40
    return [
        {"type": "session_meta", "timestamp": "2026-01-04T10:00:00.000Z",
         "payload": {"id": "0199e790-c9b9-7f41-b509-09f9694a0da2",
                     "timestamp": "2026-01-04T10:00:00.000Z",
                     "cwd": "/Users/me/Projects/personal/widget-shop",
                     "originator": "codex_cli_rs", "cli_version": "0.149.1",
                     "instructions": None, "source": {"type": "cli"}}},
        {"type": "turn_context", "timestamp": "2026-01-04T10:00:00.500Z",
         "payload": {"cwd": "/Users/me/Projects/personal/widget-shop", "approval_policy": "on-request",
                     "sandbox_policy": {"mode": "workspace-write"}, "model": "gpt-5.4", "summary": "auto"}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:01.000Z",
         "payload": {"type": "message", "role": "user",
                     "content": [{"type": "input_text",
                                  "text": "<environment_context>cwd=/Users/me</environment_context>"}]}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:02.000Z",
         "payload": {"type": "message", "role": "user",
                     "content": [{"type": "input_text",
                                  "text": "Trace why the packer emits a duplicate manifest entry."}]}},
        # The event_msg twin of the record above; ignored so nothing is indexed twice.
        {"type": "event_msg", "timestamp": "2026-01-04T10:00:02.001Z",
         "payload": {"type": "user_message", "kind": "plain",
                     "message": "Trace why the packer emits a duplicate manifest entry."}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:03.000Z",
         "payload": {"type": "reasoning", "summary": [], "content": [],
                     "encrypted_content": "opaque"}},
        {"type": "event_msg", "timestamp": "2026-01-04T10:00:03.100Z",
         "payload": {"type": "agent_reasoning", "text": "internal reasoning, not indexed"}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:04.000Z",
         "payload": {"type": "function_call", "name": "shell", "call_id": "call_1",
                     "arguments": json.dumps({"cmd": "rg -n 'manifest' packer/"})}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:05.000Z",
         "payload": {"type": "function_call_output", "call_id": "call_1", "output": long_output}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:06.000Z",
         "payload": {"type": "function_call", "name": "read_file", "call_id": "call_2",
                     "arguments": json.dumps({"path": "packer/manifest.rs"})}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:07.000Z",
         "payload": {"type": "function_call", "name": "apply_patch", "call_id": "call_3",
                     "arguments": "not valid json at all"}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:08.000Z",
         "payload": {"type": "message", "role": "assistant",
                     "content": [{"type": "output_text",
                                  "text": "The packer walks the tree twice — once for assets, once for the café ☕ icons."}]}},
        {"type": "event_msg", "timestamp": "2026-01-04T10:00:08.001Z",
         "payload": {"type": "agent_message",
                     "message": "The packer walks the tree twice — once for assets, once for the café ☕ icons."}},
        {"type": "response_item", "timestamp": "2026-01-04T10:00:09.000Z",
         "payload": {"type": "message", "role": "developer",
                     "content": [{"type": "input_text", "text": "developer role is not indexed"}]}},
        {"type": "event_msg", "timestamp": "2026-01-04T10:00:10.000Z",
         "payload": {"type": "token_count", "info": {"total": 900}, "rate_limits": {}}},
        {"type": "compacted", "timestamp": "2026-01-04T10:10:00.000Z",
         "payload": {"message": "conversation compacted", "replacement_history": []}},
        {"type": "response_item", "timestamp": "2026-01-04T10:11:00.000Z",
         "payload": {"type": "message", "role": "user",
                     "content": [{"type": "input_text",
                                  "text": "Deploy needs api_key=FIXTURE-NOT-A-SECRET-aaa and mongodb://FIXTURE-NOT-A-SECRET-bbb/db"}]}},
        {"type": "response_item", "timestamp": "2026-01-04T10:11:05.000Z",
         "payload": {"type": "custom_tool_call", "call_id": "call_4", "name": "browser",
                     "status": "completed", "input": "{}"}},
        {"type": "response_item", "timestamp": "2026-01-04T10:11:06.000Z",
         "payload": {"type": "custom_tool_call_output", "call_id": "call_4", "output": "ok"}},
        {"type": "response_item", "timestamp": "2026-01-04T10:11:10.000Z",
         "payload": {"type": "message", "role": "assistant",
                     "content": [{"type": "output_text", "text": "Store both in the keychain instead."}]}},
        {"type": "response_item", "timestamp": "2026-01-04T10:11:11.000Z",
         "payload": {"type": "function_call_output", "call_id": "call_5",
                     "output": [{"kind": "text", "value": "structured output, not a string"}]}},
    ]


def codex_short_session() -> list:
    return [
        {"type": "session_meta", "timestamp": "2026-01-05T08:00:00.000Z",
         "payload": {"id": "0199f001-1111-7222-8333-444455556666",
                     "cwd": "/Users/me/scratch", "originator": "codex_cli_rs",
                     "cli_version": "0.149.1", "source": {"type": "cli"}}},
        {"type": "response_item", "timestamp": "2026-01-05T08:00:01.000Z",
         "payload": {"type": "message", "role": "user",
                     "content": [{"type": "input_text", "text": "One question, no tools."}]}},
        {"type": "response_item", "timestamp": "2026-01-05T08:00:02.000Z",
         "payload": {"type": "message", "role": "assistant",
                     "content": [{"type": "output_text", "text": "One answer."}]}},
    ]


def main() -> int:
    write(ROOT / "claude_code" / "-Users-me-Projects-personal-widget-shop"
          / "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b.jsonl", claude_main_session())
    session_dir = ROOT / "claude_code" / "-Users-me-Projects-personal-widget-shop" / "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b"
    write(session_dir / "subagents" / "agent-a1b2c3d4e5f6a7b.jsonl", claude_subagent_session())
    write(session_dir / "subagents" / "workflows" / "wf_0a1b2c3d-e4f" / "agent-b7c6d5e4f3a2b1a.jsonl",
          claude_subagent_session())
    write(ROOT / "codex" / "2026" / "01" / "04"
          / "rollout-2026-01-04T10-00-00-0199e790-c9b9-7f41-b509-09f9694a0da2.jsonl", codex_session())
    write(ROOT / "codex" / "2026" / "01" / "05"
          / "rollout-2026-01-05T08-00-00-0199f001-1111-7222-8333-444455556666.jsonl", codex_short_session())
    return 0


if __name__ == "__main__":
    sys.exit(main())
