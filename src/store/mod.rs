//! Versioned, disposable index stores.
//!
//! Each store is a numbered directory holding a tantivy index and a SQLite
//! database. `current` is a symlink to the live one, and replacing a symlink is
//! a single atomic rename, so switching stores is never half-done. Nothing in a
//! store is irreplaceable: deleting the lot costs one reindex.

pub mod index;
pub mod meta;

use crate::paths;
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tantivy::Index;

/// Written once a store is fully built. A store without it was interrupted and
/// is deleted at the next startup.
const COMPLETE_MARKER: &str = "meta.complete";

pub struct Store {
    pub dir: PathBuf,
    pub index: Index,
    pub fields: index::Fields,
    pub db: Connection,
}

pub fn index_dir(store_dir: &Path) -> PathBuf {
    store_dir.join("tantivy")
}

pub fn db_path(store_dir: &Path) -> PathBuf {
    store_dir.join("meta.db")
}

pub fn open(store_dir: &Path) -> Result<Store> {
    let (index, fields) = index::open_or_create(&index_dir(store_dir))?;
    let db = meta::open(&db_path(store_dir))?;
    Ok(Store { dir: store_dir.to_path_buf(), index, fields, db })
}

/// Opens the store `current` points at, refusing one built by a newer binary.
pub fn open_current() -> Result<Store> {
    let dir = current_store_dir()?.with_context(|| {
        format!("no store is current at {}", paths::current_store_link().unwrap_or_default().display())
    })?;
    let store = open(&dir)?;
    check_version(&store)?;
    Ok(store)
}

/// A newer store than this binary understands is a permanent failure: the daemon
/// says so and exits rather than rewriting someone else's data.
pub fn check_version(store: &Store) -> Result<()> {
    let stored: i64 = meta::get_meta(&store.db, meta::KEY_SCHEMA_VERSION)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(meta::SCHEMA_VERSION);
    if stored > meta::SCHEMA_VERSION {
        bail!(
            "store {} has schema version {} but this binary understands {}. \
             Install the newer binary, or point `current` at an older store.",
            store.dir.display(),
            stored,
            meta::SCHEMA_VERSION
        );
    }
    Ok(())
}

pub fn stamp_version(store: &Store) -> Result<()> {
    let (schema, _) = index::build_schema();
    meta::set_meta(&store.db, meta::KEY_SCHEMA_VERSION, &meta::SCHEMA_VERSION.to_string())?;
    meta::set_meta(&store.db, meta::KEY_TANTIVY_SCHEMA_HASH, &index::schema_fingerprint(&schema))?;
    meta::set_meta(&store.db, meta::KEY_BINARY_VERSION, env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

/// The next unused number under `stores/`.
pub fn create_new_store_dir() -> Result<PathBuf> {
    let stores = paths::stores_dir()?;
    std::fs::create_dir_all(&stores)
        .with_context(|| format!("creating {}", stores.display()))?;
    let next = highest_store_number(&stores)? + 1;
    let dir = stores.join(next.to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn highest_store_number(stores: &Path) -> Result<u64> {
    let mut highest = 0u64;
    let Ok(entries) = std::fs::read_dir(stores) else { return Ok(0) };
    for entry in entries.flatten() {
        if let Some(number) = entry.file_name().to_str().and_then(|n| n.parse::<u64>().ok()) {
            highest = highest.max(number);
        }
    }
    Ok(highest)
}

pub fn is_complete(store_dir: &Path) -> bool {
    store_dir.join(COMPLETE_MARKER).exists()
}

pub fn mark_complete(store_dir: &Path) -> Result<()> {
    std::fs::write(store_dir.join(COMPLETE_MARKER), b"complete\n")
        .with_context(|| format!("marking {} complete", store_dir.display()))?;
    Ok(())
}

/// Points `current` at `store_dir` by renaming a freshly made symlink over it.
pub fn switch_current(store_dir: &Path) -> Result<()> {
    let link = paths::current_store_link()?;
    let staging = link.with_extension("switching");
    let _ = std::fs::remove_file(&staging);
    std::os::unix::fs::symlink(store_dir, &staging)
        .with_context(|| format!("creating the staging symlink {}", staging.display()))?;
    std::fs::rename(&staging, &link)
        .with_context(|| format!("moving the symlink into place at {}", link.display()))?;
    Ok(())
}

/// The absolute directory `current` points at, or `None` when the symlink is
/// missing. Always absolute so callers can compare paths directly.
pub fn current_store_dir() -> Result<Option<PathBuf>> {
    let link = paths::current_store_link()?;
    let Ok(target) = std::fs::read_link(&link) else { return Ok(None) };
    if target.is_absolute() {
        Ok(Some(target))
    } else {
        Ok(Some(link.parent().unwrap_or(Path::new(".")).join(target)))
    }
}

/// Deletes only what is certainly ours and certainly unused: incomplete stores,
/// and complete stores at this binary's schema version that are neither
/// `current` nor the recorded previous store. A store with an unrecognised
/// schema version belongs to a newer binary and is left alone.
pub fn prune_stores() -> Result<Vec<PathBuf>> {
    let stores = paths::stores_dir()?;
    let current = current_store_dir()?;
    let previous = std::fs::read_to_string(paths::previous_store_marker()?)
        .ok()
        .map(|text| PathBuf::from(text.trim().to_string()));

    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(&stores) else { return Ok(removed) };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || dir.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u64>().ok()).is_none() {
            continue;
        }
        if same_dir(&current, &dir) || same_dir(&previous, &dir) {
            continue;
        }
        if !is_complete(&dir) {
            std::fs::remove_dir_all(&dir).ok();
            removed.push(dir);
            continue;
        }
        if store_schema_version(&dir) == Some(meta::SCHEMA_VERSION) {
            std::fs::remove_dir_all(&dir).ok();
            removed.push(dir);
        }
    }
    Ok(removed)
}

fn same_dir(candidate: &Option<PathBuf>, dir: &Path) -> bool {
    candidate.as_deref() == Some(dir)
}

fn store_schema_version(store_dir: &Path) -> Option<i64> {
    let db = Connection::open(db_path(store_dir)).ok()?;
    db.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| {
        row.get::<_, String>(0)
    })
    .ok()?
    .parse()
    .ok()
}
