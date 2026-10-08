//! Turn parsing for both CLIs.
//!
//! A parser is a pure function of (bytes, context). Nothing is carried between
//! calls except the `ParseContext` the caller reconstructs from the store, so a
//! restart re-parsing the same bytes produces the same turns.

pub mod claude_code;
pub mod codex;
pub mod redact;

use serde_json::Value;
use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cli {
    ClaudeCode,
    Codex,
}

impl Cli {
    pub fn as_str(self) -> &'static str {
        match self {
            Cli::ClaudeCode => "claude_code",
            Cli::Codex => "codex",
        }
    }

    pub fn from_str(s: &str) -> Option<Cli> {
        match s {
            "claude_code" => Some(Cli::ClaudeCode),
            "codex" => Some(Cli::Codex),
            _ => None,
        }
    }
}

/// One user message plus everything that followed it, up to the next user message.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub turn_id: String,
    pub conversation_id: String,
    pub cli: Cli,
    pub session_id: String,
    pub turn_number: u64,
    pub timestamp: String,
    pub text: String,
}

/// What the store knows about a transcript. Fields that only appear in records
/// before the checkpoint (Codex `session_meta`, Claude Code's first `cwd`) are
/// carried in here rather than re-read from the file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionMeta {
    pub session_id: String,
    pub project: String,
    pub slug: String,
    pub cwd: String,
    pub git_branch: String,
    pub first_ts: String,
    pub last_ts: String,
    pub turn_count: u64,
}

pub struct ParseContext {
    pub cli: Cli,
    pub conversation_id: String,
    pub session: SessionMeta,
    pub first_turn_number: u64,
}

pub struct Parsed {
    pub closed: Vec<Turn>,
    pub open: Option<Turn>,
    /// Byte offset within the parsed slice where the open turn's first record
    /// begins. Equals `bytes.len()` when no turn is open. This is where the
    /// checkpoint moves to.
    pub open_turn_starts_at: usize,
    pub session: SessionMeta,
}

pub fn parse(bytes: &[u8], ctx: &ParseContext) -> anyhow::Result<Parsed> {
    match ctx.cli {
        Cli::ClaudeCode => claude_code::parse(bytes, ctx),
        Cli::Codex => codex::parse(bytes, ctx),
    }
}

/// A complete line of the slice, with the byte offset it starts at.
pub struct Line<'a> {
    pub offset: usize,
    pub bytes: &'a [u8],
}

/// Splits on newlines. A trailing fragment without a newline is dropped: the
/// tailer never hands over an incomplete line, so this only fires in tests.
pub fn complete_lines(bytes: &[u8]) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            out.push(Line { offset: start, bytes: &bytes[start..i] });
            start = i + 1;
        }
    }
    out
}

/// Byte offset of the last newline plus one, i.e. the end of the last complete
/// line. Zero when there is no newline at all.
pub fn cut_at_last_newline(bytes: &[u8]) -> usize {
    match bytes.iter().rposition(|b| *b == b'\n') {
        Some(i) => i + 1,
        None => 0,
    }
}

/// Python truthiness, for fields the v1 parsers test with a bare `if`.
pub fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn string_field(record: &Value, key: &str) -> String {
    record.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// The first `n` characters (not bytes), as Python slicing counts them.
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// Python's `str(value)` for a JSON value. The v1 parsers interpolate raw
/// payload fields into summaries, so a non-string field has to render the way
/// Python renders it or the text differs from the oracle.
pub fn python_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => python_repr(other),
    }
}

pub fn python_repr(v: &Value) -> String {
    let mut out = String::new();
    write_repr(v, &mut out);
    out
}

fn write_repr(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("None"),
        Value::Bool(true) => out.push_str("True"),
        Value::Bool(false) => out.push_str("False"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let _ = write!(out, "{i}");
            } else if let Some(u) = n.as_u64() {
                let _ = write!(out, "{u}");
            } else if let Some(f) = n.as_f64() {
                let _ = write!(out, "{f:?}");
            }
        }
        Value::String(s) => write_str_repr(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str_repr(k, out);
                out.push_str(": ");
                write_repr(val, out);
            }
            out.push('}');
        }
    }
}

/// Python string repr: single quotes unless the string contains a single quote
/// and no double quote.
fn write_str_repr(s: &str, out: &mut String) {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !is_python_printable(c) => {
                let code = c as u32;
                if code < 0x100 {
                    let _ = write!(out, "\\x{code:02x}");
                } else if code < 0x10000 {
                    let _ = write!(out, "\\u{code:04x}");
                } else {
                    let _ = write!(out, "\\U{code:08x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
}

/// Python's `str.isprintable()`, which drives which characters `repr` escapes:
/// everything in the Cc, Cf, Cs, Co, Zl, Zp, and Zs categories except a plain
/// space. Unassigned code points (Cn) are also non-printable to Python but are
/// not enumerated here — no transcript observed so far contains one, and
/// carrying a full Unicode category table for that case is not worth it.
fn is_python_printable(c: char) -> bool {
    let code = c as u32;
    if c == ' ' {
        return true;
    }
    let non_printable_ranges: [(u32, u32); 26] = [
        (0x0000, 0x001f), // Cc
        (0x007f, 0x009f), // Cc
        (0x00a0, 0x00a0), // Zs
        (0x00ad, 0x00ad), // Cf
        (0x0600, 0x0605), // Cf
        (0x061c, 0x061c),
        (0x06dd, 0x06dd),
        (0x070f, 0x070f),
        (0x0890, 0x0891),
        (0x08e2, 0x08e2),
        (0x1680, 0x1680), // Zs
        (0x180e, 0x180e), // Cf
        (0x2000, 0x200f), // Zs then Cf
        (0x2028, 0x202e), // Zl, Zp, Zs, Cf
        (0x205f, 0x2064), // Zs then Cf
        (0x2066, 0x206f), // Cf
        (0x3000, 0x3000), // Zs
        (0xd800, 0xdfff), // Cs
        (0xe000, 0xf8ff), // Co, the private use area
        (0xfeff, 0xfeff),
        (0xfff9, 0xfffb),
        (0x110bd, 0x110bd),
        (0x1bca0, 0x1bca3),
        (0x1d173, 0x1d17a),
        (0xe0001, 0xe007f),
        (0xf0000, 0x10fffd), // Co, the supplementary private use areas
    ];
    !non_printable_ranges.iter().any(|(low, high)| code >= *low && code <= *high)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn complete_lines_drops_trailing_fragment() {
        let lines = complete_lines(b"a\nbb\nccc");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].offset, 0);
        assert_eq!(lines[1].offset, 2);
        assert_eq!(lines[1].bytes, b"bb");
    }

    #[test]
    fn cut_at_last_newline_stops_before_partial_line() {
        assert_eq!(cut_at_last_newline(b"a\nbb\nccc"), 5);
        assert_eq!(cut_at_last_newline(b"no newline"), 0);
        assert_eq!(cut_at_last_newline(b"done\n"), 5);
    }

    #[test]
    fn python_repr_matches_cpython_forms() {
        assert_eq!(python_repr(&json!(["a", 1, null])), "['a', 1, None]");
        assert_eq!(python_repr(&json!({"k": true})), "{'k': True}");
        assert_eq!(python_repr(&json!("it's")), "\"it's\"");
        assert_eq!(python_str(&json!("plain")), "plain");
        assert_eq!(python_str(&json!(1.5)), "1.5");
    }
}
