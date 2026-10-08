//! Claude Code transcript parsing, ported from `~/.claude/mcp/conversation_search.py`.
//!
//! Turn boundaries: a `user` record whose message content is a plain string
//! starts a turn. Assistant text and tool-call summaries accumulate into it
//! until the next such record. Tool-result records (list content on a `user`
//! record) and `isMeta` records are skipped, as are slash-command echoes.

use super::{
    Cli, Line, ParseContext, Parsed, SessionMeta, Turn, complete_lines, is_truthy, python_str,
    string_field, take_chars,
};
use serde_json::Value;
use std::path::Path;

const SKIP_CONTENT_PREFIXES: [&str; 3] = [
    "<command-name>",
    "<local-command-stdout>",
    "<local-command-caveat>",
];

/// Session id and project come from the path, not from the records.
pub fn session_from_path(path: &Path) -> SessionMeta {
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    // The project is the directory directly under the transcripts root. Subagent
    // transcripts sit deeper: <project>/<session>/subagents/agent-x.jsonl, or
    // <project>/<session>/subagents/workflows/wf_x/agent-x.jsonl. Walk up past
    // those bookkeeping directories to the project.
    let project = path
        .ancestors()
        .skip(1)
        .filter_map(|p| p.file_name().and_then(|s| s.to_str()))
        .find(|name| !is_bookkeeping_dir(name))
        .unwrap_or("")
        .to_string();
    SessionMeta { session_id, project, ..SessionMeta::default() }
}

fn is_bookkeeping_dir(name: &str) -> bool {
    name == "subagents" || name == "workflows" || name.starts_with("wf_") || is_session_id(name)
}

/// 8-4-4-4-12 hex, the shape of a Claude Code session directory.
fn is_session_id(name: &str) -> bool {
    name.len() == 36
        && name.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Everything accumulating into the turn currently being built.
struct OpenTurn {
    user_text: String,
    assistant_text: String,
    tool_summaries: Vec<String>,
    timestamp: String,
    starts_at: usize,
}

pub fn parse(bytes: &[u8], ctx: &ParseContext) -> anyhow::Result<Parsed> {
    let mut session = ctx.session.clone();
    let mut closed: Vec<Turn> = Vec::new();
    let mut current: Option<OpenTurn> = None;
    let mut next_turn_number = ctx.first_turn_number;

    for line in complete_lines(bytes) {
        let Some(record) = decode(&line) else { continue };

        absorb_metadata(&record, &mut session);
        let timestamp = string_field(&record, "timestamp");

        let record_type = string_field(&record, "type");
        if record_type != "user" && record_type != "assistant" {
            continue;
        }
        if record.get("isMeta").map(is_truthy).unwrap_or(false) {
            continue;
        }
        let Some(message) = record.get("message").filter(|m| m.is_object()) else {
            continue;
        };
        let content = message.get("content");

        if record_type == "user" {
            let Some(text) = content.and_then(|c| c.as_str()) else {
                continue; // tool_result arrays and anything else do not open a turn
            };
            if SKIP_CONTENT_PREFIXES.iter().any(|p| text.starts_with(p)) {
                continue;
            }
            if let Some(open) = current.take() {
                closed.push(build_turn(open, ctx, next_turn_number));
                next_turn_number += 1;
            }
            if text.is_empty() {
                // v1 opens a turn here that it can never save, because it refuses
                // to emit a turn with no user text. Not opening one produces the
                // same turns and lets the checkpoint move past the record.
                continue;
            }
            current = Some(OpenTurn {
                user_text: text.to_string(),
                assistant_text: String::new(),
                tool_summaries: Vec::new(),
                timestamp,
                starts_at: line.offset,
            });
            continue;
        }

        // Assistant record.
        let Some(blocks) = content.and_then(|c| c.as_array()) else {
            continue;
        };
        let Some(open) = current.as_mut() else { continue };
        for block in blocks {
            if !block.is_object() {
                continue;
            }
            match string_field(block, "type").as_str() {
                "text" => {
                    let text = string_field(block, "text");
                    if !text.is_empty() {
                        open.assistant_text.push_str(&text);
                        open.assistant_text.push('\n');
                    }
                }
                "tool_use" => {
                    let name = string_field(block, "name");
                    if !name.is_empty() {
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        open.tool_summaries.push(tool_summary(&name, &input));
                    }
                }
                _ => {} // thinking blocks and anything new are ignored
            }
        }
    }

    let open_turn_starts_at = current.as_ref().map(|o| o.starts_at).unwrap_or(bytes.len());
    let open = current.map(|o| build_turn(o, ctx, next_turn_number));
    session.turn_count = next_turn_number + open.is_some() as u64;

    Ok(Parsed { closed, open, open_turn_starts_at, session })
}

fn decode(line: &Line) -> Option<Value> {
    let text = std::str::from_utf8(line.bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    serde_json::from_str(text).ok()
}

/// Session-level fields are taken from the first record that carries them, so a
/// value already in the context is never overwritten.
fn absorb_metadata(record: &Value, session: &mut SessionMeta) {
    if session.slug.is_empty() {
        session.slug = string_field(record, "slug");
    }
    if session.cwd.is_empty() {
        session.cwd = string_field(record, "cwd");
    }
    if session.git_branch.is_empty() {
        session.git_branch = string_field(record, "gitBranch");
    }
    let timestamp = string_field(record, "timestamp");
    if !timestamp.is_empty() {
        if session.first_ts.is_empty() {
            session.first_ts = timestamp.clone();
        }
        session.last_ts = timestamp;
    }
}

fn build_turn(open: OpenTurn, ctx: &ParseContext, turn_number: u64) -> Turn {
    let mut parts = vec![open.user_text];
    if !open.assistant_text.is_empty() {
        parts.push(open.assistant_text);
    }
    if !open.tool_summaries.is_empty() {
        parts.push(format!("tools: {}", open.tool_summaries.join(", ")));
    }
    Turn {
        // The turn number, not the user record's uuid: Claude Code repeats a
        // uuid across records in a handful of transcripts, and two turns sharing
        // an id would delete each other's document.
        turn_id: format!("{}:{}", ctx.conversation_id, turn_number),
        conversation_id: ctx.conversation_id.clone(),
        cli: Cli::ClaudeCode,
        session_id: ctx.session.session_id.clone(),
        turn_number,
        timestamp: open.timestamp,
        text: parts.join("\n"),
    }
}

fn tool_summary(name: &str, input: &Value) -> String {
    let Some(map) = input.as_object() else {
        return name.to_string();
    };
    let field = |key: &str| -> String {
        map.get(key).map(python_str).unwrap_or_default()
    };
    match name {
        "Bash" => {
            let command = field("command");
            if command.is_empty() {
                "Bash".to_string()
            } else {
                format!("Bash: {}", take_chars(&command, 80))
            }
        }
        "Read" => format!("Read: {}", field("file_path")),
        "Write" | "Edit" => format!("{name}: {}", field("file_path")),
        "Grep" | "Glob" => format!("{name}: {}", field("pattern")),
        "Task" => format!("Task: {}", field("description")),
        "WebSearch" => format!("WebSearch: {}", field("query")),
        "WebFetch" => format!("WebFetch: {}", field("url")),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ParseContext {
        ParseContext {
            cli: Cli::ClaudeCode,
            conversation_id: "conv".to_string(),
            session: SessionMeta { session_id: "s1".into(), project: "p".into(), ..Default::default() },
            first_turn_number: 0,
        }
    }

    #[test]
    fn user_string_record_opens_a_turn_and_assistant_text_joins_it() {
        let bytes = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"t1","message":{"role":"user","content":"hello"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
            "\n"
        )
        .as_bytes();
        let parsed = parse(bytes, &context()).unwrap();
        assert!(parsed.closed.is_empty());
        let open = parsed.open.unwrap();
        assert_eq!(open.text, "hello\nhi\n");
        assert_eq!(parsed.open_turn_starts_at, 0);
    }

    #[test]
    fn command_name_record_does_not_open_a_turn() {
        let bytes = concat!(
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/foo</command-name>"}}"#,
            "\n"
        )
        .as_bytes();
        let parsed = parse(bytes, &context()).unwrap();
        assert!(parsed.open.is_none());
        assert_eq!(parsed.open_turn_starts_at, bytes.len());
    }

    #[test]
    fn is_meta_record_is_skipped() {
        let bytes = concat!(
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}"#,
            "\n"
        )
        .as_bytes();
        assert!(parse(bytes, &context()).unwrap().open.is_none());
    }

    #[test]
    fn second_user_record_closes_the_first_turn() {
        let bytes = concat!(
            r#"{"type":"user","uuid":"a","message":{"role":"user","content":"one"}}"#,
            "\n",
            r#"{"type":"user","uuid":"b","message":{"role":"user","content":"two"}}"#,
            "\n"
        )
        .as_bytes();
        let parsed = parse(bytes, &context()).unwrap();
        assert_eq!(parsed.closed.len(), 1);
        assert_eq!(parsed.closed[0].text, "one");
        assert_eq!(parsed.closed[0].turn_number, 0);
        assert_eq!(parsed.open.as_ref().unwrap().turn_number, 1);
    }

    #[test]
    fn tool_summaries_are_appended_in_v1_format() {
        let bytes = concat!(
            r#"{"type":"user","message":{"role":"user","content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/x"}},{"type":"tool_use","name":"Glob","input":{"pattern":"*.rs"}}]}}"#,
            "\n"
        )
        .as_bytes();
        let parsed = parse(bytes, &context()).unwrap();
        assert_eq!(parsed.open.unwrap().text, "go\ntools: Read: /tmp/x, Glob: *.rs");
    }
}
