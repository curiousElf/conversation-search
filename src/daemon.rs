//! The long-running process: watch both roots, tail what changed, publish.
//!
//! One ingestion thread owns the tantivy writer and every write to the store.
//! Everything a tool call needs sits behind one lock, `Shared::published`, which
//! holds the reader, the SQLite connection, and the generation together — so a
//! call sees the complete old generation or the complete new one, never a mix.

use crate::ingest::{FileUpdate, forget_file, process_file};
use crate::paths;
use crate::reindex;
use crate::store::{self, Store, index, meta};
use crate::watch::{self, Job};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tantivy::{Index, IndexReader};

/// Commit at most this often, or this many documents, whichever comes first.
const PUBLISH_EVERY: Duration = Duration::from_secs(2);
const PUBLISH_EVERY_DOCS: usize = 500;

const WRITER_HEAP_BYTES: usize = 64 * 1024 * 1024;

/// The reader, the database, and the generation they agree on.
pub struct Published {
    pub reader: IndexReader,
    pub db: rusqlite::Connection,
    pub generation: u64,
}

pub struct Shared {
    pub published: Mutex<Published>,
    pub index: Index,
    pub fields: index::Fields,
    pub store_dir: PathBuf,
    pub roots: watch::Roots,
    pub started_at: Instant,
    pub ready: AtomicBool,
    pub pending_files: AtomicU64,
    pub build_progress: reindex::Progress,
}

impl Shared {
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

/// Opens the current store, building a first one if there is none.
pub fn open_or_build_store(roots: &watch::Roots) -> Result<(Store, reindex::Progress)> {
    let progress = reindex::Progress::default();
    if store::current_store_dir()?.is_some_and(|dir| store::is_complete(&dir)) {
        return Ok((store::open_current()?, progress));
    }
    tracing::info!("no complete store yet, building one");
    let dir = store::create_new_store_dir()?;
    let summary = reindex::build_store(&dir, roots, &progress)?;
    store::switch_current(&dir)?;
    tracing::info!(
        turns = summary.turns,
        files = summary.files,
        seconds = summary.seconds,
        "first store built"
    );
    Ok((store::open(&dir)?, progress))
}

pub fn run() -> Result<()> {
    let _log_guard = start_logging()?;
    crate::agent::clear_permanent_failure();

    let roots = watch::roots_from_home()?;
    let (store, build_progress) = match open_or_build_store(&roots) {
        Ok(pair) => pair,
        Err(error) => {
            // A store newer than this binary, or an unreadable state directory,
            // is not something a relaunch fixes. Say so and exit cleanly so
            // launchd does not loop.
            let reason = format!("{error:#}");
            tracing::error!("permanent failure: {reason}");
            crate::agent::record_permanent_failure(&reason);
            return Ok(());
        }
    };
    let Store { dir: store_dir, index, fields, db } = store;
    let reader = index::reader(&index)?;
    let generation = meta::generation(&db)?;

    let shared = Arc::new(Shared {
        published: Mutex::new(Published { reader, db, generation }),
        index,
        fields,
        store_dir,
        roots: roots.clone(),
        started_at: Instant::now(),
        ready: AtomicBool::new(false),
        pending_files: AtomicU64::new(0),
        build_progress,
    });

    let (jobs, receiver) = channel::<Job>();
    // The watcher goes in before the first reconcile, so nothing that changes
    // during the scan is missed.
    let _watcher = watch::spawn_watcher(&roots, jobs.clone())?;
    jobs.send(Job::Reconcile).ok();
    spawn_reconcile_timer(jobs.clone());

    let ingestion = {
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("ingestion".to_string())
            .spawn(move || {
                if let Err(error) = ingestion_loop(&shared, receiver) {
                    tracing::error!("ingestion stopped: {error:#}");
                }
            })
            .context("starting the ingestion thread")?
    };

    tracing::info!(store = %shared.store_dir.display(), "daemon up");
    crate::mcp::serve_blocking(Arc::clone(&shared))?;

    jobs.send(Job::Shutdown).ok();
    ingestion.join().ok();
    Ok(())
}

fn start_logging() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let dir = paths::log_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("daemon")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .context("creating the log appender")?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tantivy=warn")),
        )
        .init();
    Ok(guard)
}

fn spawn_reconcile_timer(jobs: Sender<Job>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(watch::RECONCILE_EVERY);
            if jobs.send(Job::Reconcile).is_err() {
                return;
            }
        }
    });
}

fn ingestion_loop(shared: &Shared, receiver: Receiver<Job>) -> Result<()> {
    let mut writer = shared
        .index
        .writer(WRITER_HEAP_BYTES)
        .context("creating the tantivy writer")?;
    let mut queued: BTreeSet<PathBuf> = BTreeSet::new();
    let mut pending: Vec<FileUpdate> = Vec::new();
    let mut pending_docs = 0usize;
    let mut last_publish = Instant::now();

    loop {
        match receiver.recv_timeout(Duration::from_millis(400)) {
            Ok(Job::File(path)) => {
                queued.insert(path);
            }
            Ok(Job::Reconcile) => {
                run_reconcile(shared, &mut writer, &mut queued)?;
                shared.ready.store(true, Ordering::Relaxed);
            }
            Ok(Job::Shutdown) => {
                drain(shared, &mut writer, &mut queued, &mut pending, &mut pending_docs)?;
                publish(shared, &mut writer, &mut pending)?;
                tracing::info!("ingestion stopped cleanly");
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        drain(shared, &mut writer, &mut queued, &mut pending, &mut pending_docs)?;

        let full = pending_docs >= PUBLISH_EVERY_DOCS;
        let due = !pending.is_empty() && last_publish.elapsed() >= PUBLISH_EVERY;
        if full || due {
            publish(shared, &mut writer, &mut pending)?;
            pending_docs = 0;
            last_publish = Instant::now();
        }
    }
}

fn drain(
    shared: &Shared,
    writer: &mut tantivy::IndexWriter,
    queued: &mut BTreeSet<PathBuf>,
    pending: &mut Vec<FileUpdate>,
    pending_docs: &mut usize,
) -> Result<()> {
    shared.pending_files.store(queued.len() as u64, Ordering::Relaxed);
    let batch: Vec<PathBuf> = std::mem::take(queued).into_iter().collect();
    for path in batch {
        let Some(cli) = crate::ingest::cli_for_path(&path, &shared.roots.claude, &shared.roots.codex)
        else {
            continue;
        };
        if !path.exists() {
            continue; // the next reconcile removes its documents and rows
        }
        let outcome = {
            let published = shared.published.lock().expect("the publication lock is not poisoned");
            process_file(&published.db, writer, &shared.fields, &path, cli)
        };
        match outcome {
            Ok(Some(update)) => {
                if update.restarted_from_zero {
                    tracing::info!(path = %path.display(), "identity did not hold, reindexing the file");
                }
                *pending_docs += update.turns_indexed;
                pending.push(update);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(path = %path.display(), "skipping: {error:#}"),
        }
    }
    shared.pending_files.store(queued.len() as u64, Ordering::Relaxed);
    Ok(())
}

fn run_reconcile(
    shared: &Shared,
    writer: &mut tantivy::IndexWriter,
    queued: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let started = Instant::now();
    let (changed, vanished, scanned) = {
        let published = shared.published.lock().expect("the publication lock is not poisoned");
        let result = watch::reconcile(&published.db, &shared.roots.claude, &shared.roots.codex)?;
        (result.changed, result.vanished, result.scanned)
    };
    for (path, _) in &changed {
        queued.insert(path.clone());
    }

    if !vanished.is_empty() {
        let mut published = shared.published.lock().expect("the publication lock is not poisoned");
        let tx = published.db.transaction().context("removing vanished transcripts")?;
        for path in &vanished {
            forget_file(&tx, writer, &shared.fields, path)?;
        }
        tx.commit().context("committing the removal")?;
        drop(published);
        tracing::info!(count = vanished.len(), "removed vanished transcripts");
    }

    tracing::info!(
        scanned,
        changed = changed.len(),
        vanished = vanished.len(),
        millis = started.elapsed().as_millis() as u64,
        "reconcile"
    );
    Ok(())
}

/// The publish sequence. tantivy commits first; a crash between the commit and
/// the SQLite update re-reads from the old checkpoint and re-issues the same
/// delete-and-add, which changes nothing.
fn publish(
    shared: &Shared,
    writer: &mut tantivy::IndexWriter,
    pending: &mut Vec<FileUpdate>,
) -> Result<u64> {
    if pending.is_empty() {
        return Ok(0);
    }
    writer.commit().context("committing the tantivy batch")?;

    let mut published = shared.published.lock().expect("the publication lock is not poisoned");
    let tx = published.db.transaction().context("starting the publish transaction")?;
    for update in pending.iter() {
        meta::upsert_file(&tx, &update.file)?;
        meta::upsert_session(&tx, &update.session)?;
    }
    let generation = meta::bump_generation(&tx)?;
    tx.commit().context("committing the publish transaction")?;

    published.reader.reload().context("reloading the reader")?;
    published.generation = generation;
    drop(published);

    tracing::info!(generation, files = pending.len(), "published");
    pending.clear();
    Ok(generation)
}
