//! Codex transcript parsing, ported from `~/.codex/mcp/codex_conversation_search.py`.
//!
//! Only `response_item` records carry indexed content. The parallel `event_msg`
//! records repeat the same text and are ignored so nothing is indexed twice.
//! Every piece of text passes through redaction.

use super::redact::redact;
use super::{
    Cli, Line, ParseContext, Parsed, SessionMeta, Turn, char_count, complete_lines, python_str,
    string_field, take_chars,
};
use serde_json::Value;
use std::path::Path;

const SKIP_USER_PREFIXES: [&str; 3] = [
    "<environment_context>",
    "<turn_aborted>",
    "<user_editable_context>",
];

/// `rollout-2026-05-18T15-26-53-<uuid>.jsonl` — the session id is the last five
/// dash-separated pieces. A `session_meta` record overrides this when present.
pub fn session_from_path(path: &Path) -> SessionMeta {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let mut session_id = stem.clone();
    if stem.starts_with("rollout-") && stem.contains('-') {
        let parts: Vec<&str> = stem.split('-').collect();
        if parts.len() >= 8 {
            session_id = parts[parts.len() - 5..].join("-");
        }
    }
    SessionMeta { session_id, slug: stem, ..SessionMeta::default() }
}

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
    let mut session_id = ctx.session.session_id.clone();

    for line in complete_lines(bytes) {
        let Some(record) = decode(&line) else { continue };

        let timestamp = string_field(&record, "timestamp");
        if !timestamp.is_empty() {
            if session.first_ts.is_empty() {
                session.first_ts = timestamp.clone();
            }
            session.last_ts = timestamp.clone();
        }

        let record_type = string_field(&record, "type");
        let empty = Value::Object(serde_json::Map::new());
        let payload = record.get("payload").filter(|p| p.is_object()).unwrap_or(&empty);

        if record_type == "session_meta" {
            let id = string_field(payload, "id");
            if !id.is_empty() {
                session_id = id.clone();
                session.session_id = id;
            }
            let cwd = string_field(payload, "cwd");
            if !cwd.is_empty() {
                session.cwd = cwd;
            }
            session.project = project_from_cwd(&session.cwd);
            continue;
        }
        if record_type != "response_item" {
            continue;
        }

        match string_field(payload, "type").as_str() {
            "message" => {
                let role = string_field(payload, "role");
                if role != "user" && role != "assistant" {
                    continue;
                }
                let text = shorten(&text_from_content(payload.get("content")), 4000);
                if text.is_empty() {
                    continue;
                }
                if role == "user" {
                    if SKIP_USER_PREFIXES.iter().any(|p| text.starts_with(p)) {
                        continue;
                    }
                    if let Some(open) = current.take() {
                        closed.push(build_turn(open, ctx, &session_id, next_turn_number));
                        next_turn_number += 1;
                    }
                    current = Some(OpenTurn {
                        user_text: text,
                        assistant_text: String::new(),
                        tool_summaries: Vec::new(),
                        timestamp,
                        starts_at: line.offset,
                    });
                } else if let Some(open) = current.as_mut() {
                    open.assistant_text.push_str(&text);
                    open.assistant_text.push('\n');
                }
            }
            "function_call" => {
                if let Some(open) = current.as_mut() {
                    let summary = function_call_summary(payload);
                    if !summary.is_empty() {
                        open.tool_summaries.push(summary);
                    }
                }
            }
            "function_call_output" => {
                if let Some(open) = current.as_mut() {
                    let summary = function_output_summary(payload);
                    if !summary.is_empty() {
                        open.tool_summaries.push(summary);
                    }
                }
            }
            _ => {} // reasoning, custom_tool_call, and anything new are skipped
        }
    }

    let open_turn_starts_at = current.as_ref().map(|o| o.starts_at).unwrap_or(bytes.len());
    let open = current.map(|o| build_turn(o, ctx, &session_id, next_turn_number));
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

fn build_turn(open: OpenTurn, ctx: &ParseContext, session_id: &str, turn_number: u64) -> Turn {
    let mut parts = vec![format!("user: {}", open.user_text)];
    if !open.assistant_text.is_empty() {
        parts.push(format!("assistant: {}", open.assistant_text.trim()));
    }
    if !open.tool_summaries.is_empty() {
        parts.push(format!("tools: {}", open.tool_summaries.join("\n")));
    }
    Turn {
        turn_id: format!("{}:{}", ctx.conversation_id, turn_number),
        conversation_id: ctx.conversation_id.clone(),
        cli: Cli::Codex,
        session_id: session_id.to_string(),
        turn_number,
        timestamp: open.timestamp,
        text: parts.join("\n"),
    }
}

/// Path segments after `Projects`, up to three, joined with `/`. Falls back to
/// the last segment when there is no `Projects` component.
pub fn project_from_cwd(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    let path = Path::new(cwd);
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if let Some(index) = parts.iter().position(|p| p == "Projects") {
        let end = (index + 4).min(parts.len());
        let joined = parts[index + 1..end].join("/");
        if joined.is_empty() { name } else { joined }
    } else if name.is_empty() {
        cwd.to_string()
    } else {
        name
    }
}

fn text_from_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            let mut chunks: Vec<String> = Vec::new();
            for block in blocks {
                let Some(map) = block.as_object() else { continue };
                let chosen = ["text", "input_text", "output_text"]
                    .iter()
                    .find_map(|key| map.get(*key).filter(|v| super::is_truthy(v)));
                if let Some(value) = chosen {
                    chunks.push(python_str(value));
                }
            }
            chunks.join("\n")
        }
        _ => String::new(),
    }
}

fn shorten(text: &str, limit: usize) -> String {
    let redacted = redact(text.trim());
    if char_count(&redacted) <= limit {
        redacted
    } else {
        format!("{}...", take_chars(&redacted, limit))
    }
}

fn function_call_summary(payload: &Value) -> String {
    let name = match payload.get("name").filter(|v| super::is_truthy(v)) {
        Some(value) => python_str(value),
        None => "tool".to_string(),
    };
    let raw_arguments = payload.get("arguments");

    let parsed: Option<Value> = match raw_arguments {
        Some(Value::String(s)) => serde_json::from_str(s).ok(),
        Some(other) => Some(other.clone()),
        None => None,
    };
    if let Some(map) = parsed.as_ref().and_then(|v| v.as_object()) {
        if let Some(cmd) = map.get("cmd").filter(|v| super::is_truthy(v)) {
            return format!("{name}: {}", python_str(cmd));
        }
        for key in ["path", "file_path", "ref_id"] {
            if let Some(value) = map.get(key).filter(|v| super::is_truthy(v)) {
                return format!("{name}: {}", python_str(value));
            }
        }
    }
    let raw = match raw_arguments {
        Some(v) if super::is_truthy(v) => python_str(v),
        _ => String::new(),
    };
    format!("{name}: {}", shorten(&raw, 240))
}

fn function_output_summary(payload: &Value) -> String {
    let Some(output) = payload.get("output").filter(|v| super::is_truthy(v)) else {
        return String::new();
    };
    format!("tool_output: {}", shorten(&python_str(output), 600))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ParseContext {
        ParseContext {
            cli: Cli::Codex,
            conversation_id: "conv".to_string(),
            session: SessionMeta::default(),
            first_turn_number: 0,
        }
    }

    #[test]
    fn session_id_comes_from_the_last_five_dash_parts() {
        let meta = session_from_path(Path::new(
            "/x/rollout-2026-05-18T15-26-53-019e3a84-5aa3-7851-8a68-32bbf48fd7b9.jsonl",
        ));
        assert_eq!(meta.session_id, "019e3a84-5aa3-7851-8a68-32bbf48fd7b9");
    }

    #[test]
    fn project_is_up_to_three_segments_after_projects() {
        assert_eq!(project_from_cwd("/Users/x/Projects/a/b/c/d"), "a/b/c");
        assert_eq!(project_from_cwd("/Users/x/other/thing"), "thing");
        assert_eq!(project_from_cwd(""), "");
    }

    #[test]
    fn event_msg_records_are_ignored_so_text_is_not_indexed_twice() {
        let bytes = concat!(
            r#"{"type":"response_item","timestamp":"t1","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            "\n"
        )
        .as_bytes();
        let parsed = parse(bytes, &context()).unwrap();
        assert_eq!(parsed.open.unwrap().text, "user: hello");
    }

    #[test]
    fn function_call_and_output_become_tool_lines() {
        let bytes = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"ls -l\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","output":"total 0"}}"#,
            "\n"
        )
        .as_bytes();
        let parsed = parse(bytes, &context()).unwrap();
        assert_eq!(
            parsed.open.unwrap().text,
            "user: go\ntools: shell: ls -l\ntool_output: total 0"
        );
    }

    #[test]
    fn environment_context_user_message_does_not_open_a_turn() {
        let bytes = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
            "\n"
        )
        .as_bytes();
        assert!(parse(bytes, &context()).unwrap().open.is_none());
    }
}
