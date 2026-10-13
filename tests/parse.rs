//! Parser checks over the synthetic fixtures.
//!
//! 1. Feeding a fixture in chunks cut at newlines produces the same closed turns
//!    as feeding it whole, and the open turn after each chunk matches what a
//!    whole-file parse of the bytes consumed so far reports.
//! 2. No fixture contains anything the secret patterns would redact, other
//!    than the deliberately synthetic markers.
//! 3. Subagent transcripts take the project of the folder they live in.

use conversation_search::parse::{
    Cli, ParseContext, Parsed, SessionMeta, claude_code, codex, cut_at_last_newline, parse,
    redact::redact,
};
use conversation_search::transcripts::find_transcripts;
use std::path::{Path, PathBuf};

/// The parsers only ever put the context's conversation id into a turn id, so a
/// fixed one is enough here and keeps the whole-file and chunked parses
/// comparable.
const CONVERSATION_ID: &str = "fixture";

const SYNTHETIC_MARKER: &str = "FIXTURE-NOT-A-SECRET";

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn cli_of(path: &Path) -> Cli {
    let text = path.to_string_lossy();
    if text.contains("/fixtures/codex/") { Cli::Codex } else { Cli::ClaudeCode }
}

fn context_for(path: &Path, cli: Cli) -> ParseContext {
    let session = match cli {
        Cli::ClaudeCode => claude_code::session_from_path(path),
        Cli::Codex => codex::session_from_path(path),
    };
    ParseContext {
        cli,
        conversation_id: CONVERSATION_ID.to_string(),
        session,
        first_turn_number: 0,
    }
}

fn parse_whole(path: &Path, bytes: &[u8]) -> Parsed {
    let cli = cli_of(path);
    parse(bytes, &context_for(path, cli)).expect("parse")
}

#[test]
fn chunked_parse_equals_whole_file_parse() {
    for path in find_transcripts(&fixtures_root()) {
        let cli = cli_of(&path);
        let bytes = std::fs::read(&path).expect("reading fixture");

        // `read_to` is how far the tailer has read; `checkpoint` is where the
        // open turn starts and always trails it. Reading advances, so the loop
        // terminates even when the open turn never closes.
        let mut read_to = 0usize;
        let mut checkpoint = 0usize;
        let mut turn_number = 0u64;
        let mut closed_so_far: Vec<_> = Vec::new();
        let mut open_now = None;
        let mut session = context_for(&path, cli).session;

        while read_to < bytes.len() {
            read_to = next_chunk_end(&bytes, read_to, 4096);
            let slice = &bytes[checkpoint..read_to];
            let consumed = cut_at_last_newline(slice);
            if consumed == 0 {
                continue;
            }

            let ctx = ParseContext {
                cli,
                conversation_id: CONVERSATION_ID.to_string(),
                session: session.clone(),
                first_turn_number: turn_number,
            };
            let step = parse(&slice[..consumed], &ctx).expect("chunk parse");
            let consumed_end = checkpoint + consumed;

            // The same bytes fed whole must report the same history and the
            // same open turn.
            let whole = parse_whole(&path, &bytes[..consumed_end]);
            closed_so_far.extend(step.closed.iter().cloned());
            assert_eq!(
                closed_so_far,
                whole.closed,
                "closed turns diverge at byte {} of {}",
                consumed_end,
                path.display()
            );
            assert_eq!(
                step.open,
                whole.open,
                "open turn diverges at byte {} of {}",
                consumed_end,
                path.display()
            );

            turn_number += step.closed.len() as u64;
            session = step.session;
            open_now = step.open;
            checkpoint += step.open_turn_starts_at;
            assert!(checkpoint <= consumed_end);
        }

        closed_so_far.extend(open_now);
        let whole = parse_whole(&path, &bytes);
        let mut expected = whole.closed.clone();
        expected.extend(whole.open.iter().cloned());
        assert_eq!(closed_so_far, expected, "final turns differ for {}", path.display());
    }
}

/// End of the chunk starting at `from`: `size` bytes, extended forward to the
/// next newline so the chunk always contains at least one complete line.
fn next_chunk_end(bytes: &[u8], from: usize, size: usize) -> usize {
    let target = (from + size).min(bytes.len());
    if bytes[from..target].contains(&b'\n') {
        return target;
    }
    match bytes[target..].iter().position(|b| *b == b'\n') {
        Some(offset) => target + offset + 1,
        None => bytes.len(),
    }
}

#[test]
fn fixtures_contain_no_secret_shaped_text() {
    for path in find_transcripts(&fixtures_root()) {
        let contents = std::fs::read_to_string(&path).expect("reading fixture");
        for (number, line) in contents.lines().enumerate() {
            if redact(line) != line {
                assert!(
                    line.contains(SYNTHETIC_MARKER),
                    "{}:{} matches a secret pattern without the {} marker",
                    path.display(),
                    number + 1,
                    SYNTHETIC_MARKER
                );
            }
        }
    }
}

#[test]
fn subagent_transcripts_take_the_project_directory() {
    let project = "-Users-me-Projects-personal-widget-shop";
    let session = "1f2e3d4c-5b6a-7089-9a8b-7c6d5e4f3a2b";
    for rel in [
        format!("claude_code/{project}/{session}.jsonl"),
        format!("claude_code/{project}/{session}/subagents/agent-a1b2c3d4e5f6a7b.jsonl"),
        format!("claude_code/{project}/{session}/subagents/workflows/wf_0a1b2c3d-e4f/agent-b7c6d5e4f3a2b1a.jsonl"),
    ] {
        let meta: SessionMeta = claude_code::session_from_path(&fixtures_root().join(&rel));
        assert_eq!(meta.project, project, "for {rel}");
    }
}
