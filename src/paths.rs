//! Where everything lives. Nothing here points inside `~/.claude` or
//! `~/.codex` except the two transcript roots, which are only ever read.

use anyhow::{Context, Result};
use std::path::PathBuf;

pub const PORT: u16 = 7317;
pub const LAUNCHD_LABEL: &str = "io.github.curiouself.conversation-search";
pub const HEALTH_NAME: &str = "conversation-search";

pub fn home() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home))
}

pub fn claude_transcript_root() -> Result<PathBuf> {
    Ok(home()?.join(".claude/projects"))
}

pub fn codex_transcript_root() -> Result<PathBuf> {
    Ok(home()?.join(".codex/sessions"))
}

pub fn state_dir() -> Result<PathBuf> {
    Ok(home()?.join("Library/Application Support/conversation-search"))
}

pub fn stores_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("stores"))
}

pub fn current_store_link() -> Result<PathBuf> {
    Ok(stores_dir()?.join("current"))
}

/// Records which store `current` pointed at before an install, so a rollback can
/// put it back.
pub fn previous_store_marker() -> Result<PathBuf> {
    Ok(state_dir()?.join("store.prev"))
}

pub fn log_dir() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/conversation-search"))
}

pub fn plist_path() -> Result<PathBuf> {
    Ok(home()?.join(format!("Library/LaunchAgents/{LAUNCHD_LABEL}.plist")))
}

pub fn health_url() -> String {
    format!("http://127.0.0.1:{PORT}/healthz")
}
