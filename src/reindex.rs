//! Building a complete store from scratch.
//!
//! Offline: the caller stops the daemon, builds into a fresh numbered store,
//! then switches the `current` symlink. A crash part-way leaves `current` where
//! it was and the half-built store without its `meta.complete` marker, so the
//! next startup deletes it.

use crate::ingest::process_file;
use crate::store::{self, meta};
use crate::transcripts::find_transcripts;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Live counters, so `/healthz` can report progress while a build runs.
#[derive(Default)]
pub struct Progress {
    pub files_total: AtomicU64,
    pub files_done: AtomicU64,
    pub turns_indexed: AtomicU64,
}

pub struct Summary {
    pub files: u64,
    pub turns: u64,
    pub seconds: f64,
}

/// Commit every this many documents. Small enough that a crash loses little
/// work, large enough that tantivy is not committing constantly.
const COMMIT_EVERY_DOCS: usize = 2_000;

const WRITER_HEAP_BYTES: usize = 128 * 1024 * 1024;

pub fn build_store(
    store_dir: &Path,
    roots: &crate::watch::Roots,
    progress: &Progress,
) -> Result<Summary> {
    let started = Instant::now();
    let mut store = store::open(store_dir)?;
    store::stamp_version(&store)?;

    let mut writer = store
        .index
        .writer(WRITER_HEAP_BYTES)
        .context("creating the tantivy writer")?;

    let mut files: Vec<(PathBuf, crate::parse::Cli)> = find_transcripts(&roots.claude)
        .into_iter()
        .map(|path| (path, crate::parse::Cli::ClaudeCode))
        .collect();
    files.extend(
        find_transcripts(&roots.codex).into_iter().map(|path| (path, crate::parse::Cli::Codex)),
    );
    progress.files_total.store(files.len() as u64, Ordering::Relaxed);

    let mut pending: Vec<crate::ingest::FileUpdate> = Vec::new();
    let mut pending_docs = 0usize;
    let mut total_turns = 0u64;

    for (path, cli) in &files {
        match process_file(&store.db, &writer, &store.fields, path, *cli) {
            Ok(Some(update)) => {
                pending_docs += update.turns_indexed;
                total_turns += update.turns_indexed as u64;
                pending.push(update);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(path = %path.display(), "skipping unreadable transcript: {error:#}");
            }
        }
        progress.files_done.fetch_add(1, Ordering::Relaxed);
        progress.turns_indexed.store(total_turns, Ordering::Relaxed);

        if pending_docs >= COMMIT_EVERY_DOCS {
            publish(&mut store, &mut writer, &mut pending)?;
            pending_docs = 0;
        }
    }
    publish(&mut store, &mut writer, &mut pending)?;

    store::mark_complete(store_dir)?;
    Ok(Summary {
        files: files.len() as u64,
        turns: total_turns,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// Commit the tantivy batch first, then record the checkpoints it covers. A
/// crash between the two re-reads from the old checkpoint and re-issues the same
/// delete-and-add, which is a no-op.
pub fn publish(
    store: &mut store::Store,
    writer: &mut tantivy::IndexWriter,
    pending: &mut Vec<crate::ingest::FileUpdate>,
) -> Result<u64> {
    if pending.is_empty() {
        return meta::generation(&store.db);
    }
    writer.commit().context("committing the tantivy batch")?;

    let tx = store.db.transaction().context("starting the publish transaction")?;
    for update in pending.iter() {
        meta::upsert_file(&tx, &update.file)?;
        meta::upsert_session(&tx, &update.session)?;
    }
    let generation = meta::bump_generation(&tx)?;
    tx.commit().context("committing the publish transaction")?;

    pending.clear();
    Ok(generation)
}
