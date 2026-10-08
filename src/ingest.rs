//! Turning one transcript file into index documents.
//!
//! The order is always: identity check, read from the checkpoint, cut at the
//! last newline, parse, delete-then-add each turn. Deterministic turn ids make
//! that idempotent, so re-running the same bytes leaves the index unchanged.
//!
//! This function never commits and never writes SQLite. The caller batches both,
//! which is what makes the publish ordering in `serve` possible.

use crate::parse::{Cli, ParseContext, SessionMeta, claude_code, codex, cut_at_last_newline, parse};
use crate::store::{index, meta};
use crate::transcripts::{open_transcript, prefix_hash, read_from, stat};
use anyhow::{Context, Result};
use std::path::Path;
use tantivy::IndexWriter;

/// What the caller must write once the batch commits.
pub struct FileUpdate {
    pub file: meta::FileRow,
    pub session: meta::SessionRow,
    pub turns_indexed: usize,
    /// True when the file's identity did not hold and its documents were dropped.
    pub restarted_from_zero: bool,
}

/// BLAKE3 of (cli, path), truncated to 16 bytes, hex. Opaque to clients: results
/// carry `cli`, `session_id` and `turn_number` as their own fields, so nothing
/// outside this file parses it. Stable across rewrites of the same path, and
/// different for a copy of a transcript at another path, so the two never delete
/// each other's documents.
fn conversation_id(cli: Cli, path: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(cli.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.finalize().to_hex()[..32].to_string()
}

pub fn cli_for_path(path: &Path, claude_root: &Path, codex_root: &Path) -> Option<Cli> {
    if path.starts_with(claude_root) {
        Some(Cli::ClaudeCode)
    } else if path.starts_with(codex_root) {
        Some(Cli::Codex)
    } else {
        None
    }
}

fn session_from_path(cli: Cli, path: &Path) -> SessionMeta {
    match cli {
        Cli::ClaudeCode => claude_code::session_from_path(path),
        Cli::Codex => codex::session_from_path(path),
    }
}

fn session_from_row(row: &meta::SessionRow) -> SessionMeta {
    SessionMeta {
        session_id: row.session_id.clone(),
        project: row.project.clone(),
        slug: row.slug.clone(),
        cwd: row.cwd.clone(),
        git_branch: row.git_branch.clone(),
        first_ts: row.first_ts.clone(),
        last_ts: row.last_ts.clone(),
        turn_count: row.turn_count,
    }
}

/// Indexes everything new in one file. Returns `None` when the file has not
/// changed since it was last indexed.
pub fn process_file(
    db: &rusqlite::Connection,
    writer: &IndexWriter,
    fields: &index::Fields,
    path: &Path,
    cli: Cli,
) -> Result<Option<FileUpdate>> {
    let path_text = path.to_string_lossy().to_string();
    let mut file = open_transcript(path)?;
    let file_stat = stat(&file)?;
    let existing = meta::get_file(db, &path_text)?;

    let conversation = conversation_id(cli, path);
    let mut start = 0u64;
    let mut first_turn_number = 0u64;
    let mut session = session_from_path(cli, path);
    let mut restarted_from_zero = true;

    if let Some(row) = existing.as_ref() {
        let identity_holds = row.dev == file_stat.dev
            && row.ino == file_stat.ino
            && file_stat.size >= row.checkpoint
            && prefix_hash(&mut file, row.checkpoint)? == row.prefix_hash;
        if identity_holds {
            if file_stat.size == row.size && file_stat.mtime == row.mtime {
                return Ok(None);
            }
            start = row.checkpoint;
            first_turn_number = row.checkpoint_turn_number;
            restarted_from_zero = false;
            if let Some(stored) = meta::session_by_conversation(db, &row.conversation_id)? {
                session = session_from_row(&stored);
            }
        }
    }

    if restarted_from_zero {
        // Either a first sighting or a file that is no longer the one we
        // indexed. Dropping the path's documents makes the rebuild exact.
        index::delete_source_path(writer, fields, &path_text);
    }

    let buffer = read_from(&mut file, start)?;
    let consumed = cut_at_last_newline(&buffer);

    let ctx = ParseContext {
        cli,
        conversation_id: conversation.clone(),
        session,
        first_turn_number,
    };
    let parsed = parse(&buffer[..consumed], &ctx)
        .with_context(|| format!("parsing {}", path.display()))?;

    let project = parsed.session.project.clone();
    let mut turns_indexed = 0usize;
    for turn in parsed.closed.iter().chain(parsed.open.iter()) {
        index::delete_turn(writer, fields, &turn.turn_id);
        writer
            .add_document(index::document_for(fields, turn, &project, &path_text))
            .with_context(|| format!("indexing a turn of {}", path.display()))?;
        turns_indexed += 1;
    }

    let checkpoint = start + parsed.open_turn_starts_at as u64;
    let checkpoint_turn_number = first_turn_number + parsed.closed.len() as u64;
    let new_prefix_hash = prefix_hash(&mut file, checkpoint)?;

    let file_row = meta::FileRow {
        path: path_text.clone(),
        cli: cli.as_str().to_string(),
        conversation_id: conversation.clone(),
        dev: file_stat.dev,
        ino: file_stat.ino,
        size: file_stat.size,
        mtime: file_stat.mtime,
        checkpoint,
        checkpoint_turn_number,
        prefix_hash: new_prefix_hash,
        last_indexed_at: now_seconds(),
    };
    let session_row = meta::SessionRow {
        conversation_id: conversation,
        cli: cli.as_str().to_string(),
        session_id: parsed.session.session_id.clone(),
        source_path: path_text,
        project,
        cwd: parsed.session.cwd.clone(),
        git_branch: parsed.session.git_branch.clone(),
        slug: parsed.session.slug.clone(),
        first_ts: parsed.session.first_ts.clone(),
        last_ts: parsed.session.last_ts.clone(),
        turn_count: parsed.session.turn_count,
    };

    Ok(Some(FileUpdate {
        file: file_row,
        session: session_row,
        turns_indexed,
        restarted_from_zero,
    }))
}

/// Drops everything the index and the store know about a path. Used when a
/// transcript disappears.
pub fn forget_file(
    tx: &rusqlite::Transaction,
    writer: &IndexWriter,
    fields: &index::Fields,
    path: &str,
) -> Result<()> {
    index::delete_source_path(writer, fields, path);
    meta::delete_file(tx, path)?;
    Ok(())
}

pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
