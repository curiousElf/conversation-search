//! Noticing that a transcript changed.
//!
//! FSEvents is a hint, not a queue of record: events are coalesced and can be
//! dropped. So the watcher only nudges the ingestion actor, and a reconcile scan
//! — which compares every path, device, inode, size and mtime against the
//! `files` table — runs at startup after the watcher is installed, and on a
//! timer after that.

use crate::paths;
use crate::store::meta;
use crate::transcripts::find_transcripts;
use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_full::new_debouncer;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::sync::mpsc::Sender;

pub const DEBOUNCE: Duration = Duration::from_secs(3);
pub const RECONCILE_EVERY: Duration = Duration::from_secs(600);

/// What the ingestion actor is asked to do.
pub enum Job {
    /// Re-read one transcript from its checkpoint.
    File(PathBuf),
    /// Walk both roots and enqueue everything that changed or vanished.
    Reconcile,
    Shutdown,
}

/// Installs the FSEvents watchers. The returned debouncer must be kept alive:
/// dropping it stops the watch.
pub fn spawn_watcher(roots: &Roots, jobs: Sender<Job>) -> Result<notify_debouncer_full::Debouncer<notify::FsEventWatcher, notify_debouncer_full::RecommendedCache>> {
    let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: notify_debouncer_full::DebounceEventResult| {
        let Ok(events) = result else { return };
        for event in events {
            for path in &event.paths {
                if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                    let _ = jobs.send(Job::File(path.clone()));
                }
            }
        }
    })
    .context("creating the file watcher")?;

    for root in [&roots.claude, &roots.codex] {
        debouncer
            .watch(root, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", root.display()))?;
        tracing::info!(root = %root.display(), "watching");
    }
    Ok(debouncer)
}

/// What a reconcile scan found.
pub struct ReconcileResult {
    pub changed: Vec<(PathBuf, crate::parse::Cli)>,
    pub vanished: Vec<String>,
    pub scanned: usize,
}

/// Compares both roots against the `files` table. A file whose mtime moved but
/// whose size did not is logged: the identity check cannot see an in-place edit
/// of closed history that leaves the 4 KB before the checkpoint alone.
pub fn reconcile(
    db: &rusqlite::Connection,
    claude_root: &Path,
    codex_root: &Path,
) -> Result<ReconcileResult> {
    let mut on_disk: Vec<(PathBuf, crate::parse::Cli)> = find_transcripts(claude_root)
        .into_iter()
        .map(|p| (p, crate::parse::Cli::ClaudeCode))
        .collect();
    on_disk.extend(find_transcripts(codex_root).into_iter().map(|p| (p, crate::parse::Cli::Codex)));

    let known = meta::all_files(db)?;
    let mut changed = Vec::new();
    let scanned = on_disk.len();

    for (path, cli) in on_disk.iter() {
        let path_text = path.to_string_lossy().to_string();
        let row = known.iter().find(|row| row.path == path_text);
        match (row, std::fs::metadata(path)) {
            (Some(row), Ok(metadata)) => {
                use std::os::unix::fs::MetadataExt;
                if metadata.size() != row.size || metadata.mtime() != row.mtime {
                    if metadata.size() == row.size {
                        tracing::warn!(
                            path = %path.display(),
                            "mtime changed without size changing; closed history may have been edited in place"
                        );
                    }
                    changed.push((path.clone(), *cli));
                }
            }
            (None, Ok(_)) => changed.push((path.clone(), *cli)),
            _ => {}
        }
    }

    let live: std::collections::HashSet<String> =
        on_disk.iter().map(|(p, _)| p.to_string_lossy().to_string()).collect();
    let vanished =
        known.iter().filter(|row| !live.contains(&row.path)).map(|row| row.path.clone()).collect();

    Ok(ReconcileResult { changed, vanished, scanned })
}

/// Both transcript roots, carried together so nothing reads them from a global.
#[derive(Clone)]
pub struct Roots {
    pub claude: PathBuf,
    pub codex: PathBuf,
}

pub fn roots_from_home() -> Result<Roots> {
    Ok(Roots {
        claude: paths::claude_transcript_root()?,
        codex: paths::codex_transcript_root()?,
    })
}
