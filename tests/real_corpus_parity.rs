//! Parity against a sample of the real corpus.
//!
//! Ignored by default because it depends on the machine's transcripts and takes
//! a while. It is the check that catches record shapes the synthetic fixtures do
//! not cover. Transcripts are only ever read.
//!
//!     cargo test --test real_corpus_parity -- --ignored --nocapture

use conversation_search::parse::{Cli, ParseContext, claude_code, codex, parse};
use conversation_search::transcripts::find_transcripts;
use std::path::Path;
use std::process::Command;

/// Only the turn text, count and project are compared, so a fixed conversation
/// id is enough.
const CONVERSATION_ID: &str = "fixture";

/// Every 40th Claude Code transcript, every 11th Codex one. Raising these to 1
/// compares the whole corpus and takes about 90 seconds.
const CLAUDE_STRIDE: usize = 40;
const CODEX_STRIDE: usize = 11;

#[derive(serde::Deserialize)]
struct OracleOutput {
    turn_count: usize,
    project: String,
    turns: Vec<OracleTurn>,
}

#[derive(serde::Deserialize)]
struct OracleTurn {
    turn_number: u64,
    text: String,
}

fn run_oracle(path: &Path, cli: Cli) -> Option<OracleOutput> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("uv")
        .current_dir(root)
        .args(["run", "--no-project", "python", "fixtures/oracle.py", cli.as_str()])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn compare(cli: Cli, root: &Path, stride: usize) -> (usize, usize) {
    let mut checked = 0usize;
    let mut mismatched = 0usize;
    for (index, path) in find_transcripts(root).into_iter().enumerate() {
        if index % stride != 0 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let session = match cli {
            Cli::ClaudeCode => claude_code::session_from_path(&path),
            Cli::Codex => codex::session_from_path(&path),
        };
        let ctx = ParseContext {
            cli,
            conversation_id: CONVERSATION_ID.to_string(),
            session,
            first_turn_number: 0,
        };
        let parsed = parse(&bytes, &ctx).expect("parse");
        let Some(oracle) = run_oracle(&path, cli) else {
            continue;
        };

        let mut ours: Vec<_> = parsed.closed.iter().collect();
        ours.extend(parsed.open.iter());
        checked += 1;

        let mut bad = false;
        // v1 derives the wrong project for subagent transcripts; only top-level files compare it.
        let project_differs = !path.to_string_lossy().contains("/subagents/")
            && parsed.session.project != oracle.project;
        if ours.len() != oracle.turn_count || project_differs {
            bad = true;
            println!(
                "MISMATCH {} — turns {} vs {}, project {:?} vs {:?}",
                path.display(),
                ours.len(),
                oracle.turn_count,
                parsed.session.project,
                oracle.project
            );
        } else {
            for (mine, theirs) in ours.iter().zip(oracle.turns.iter()) {
                if mine.text != theirs.text {
                    bad = true;
                    println!("MISMATCH {} turn {}", path.display(), theirs.turn_number);
                    println!("  ours:   {:?}", &mine.text.chars().take(200).collect::<String>());
                    println!("  oracle: {:?}", &theirs.text.chars().take(200).collect::<String>());
                    break;
                }
            }
        }
        if bad {
            mismatched += 1;
        }
    }
    (checked, mismatched)
}

#[test]
#[ignore = "reads the machine's real transcripts"]
fn real_transcripts_match_v1() {
    let home = std::env::var("HOME").expect("HOME");

    let (claude_checked, claude_bad) =
        compare(Cli::ClaudeCode, &Path::new(&home).join(".claude/projects"), CLAUDE_STRIDE);
    println!("claude_code: {claude_checked} files checked, {claude_bad} mismatched");

    let (codex_checked, codex_bad) =
        compare(Cli::Codex, &Path::new(&home).join(".codex/sessions"), CODEX_STRIDE);
    println!("codex: {codex_checked} files checked, {codex_bad} mismatched");

    assert_eq!(claude_bad + codex_bad, 0, "parsers diverge from v1 on real transcripts");
}
