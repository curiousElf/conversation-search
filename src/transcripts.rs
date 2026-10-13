//! The only door to transcript files.
//!
//! Every read of `~/.claude/projects` or `~/.codex/sessions` goes through
//! `open_transcript`, which opens with read-only flags. No code path in this
//! crate opens a transcript for writing, renames one, or unlinks one.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub fn open_transcript(path: &Path) -> Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(false)
        .create(false)
        .append(false)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening transcript {}", path.display()))
}

/// Device, inode, size, and mtime as the identity check needs them.
pub struct FileStat {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
}

pub fn stat(file: &File) -> Result<FileStat> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().context("stat on an open transcript")?;
    Ok(FileStat { dev: meta.dev(), ino: meta.ino(), size: meta.size(), mtime: meta.mtime() })
}

/// Bytes from `offset` to end of file.
pub fn read_from(file: &mut File, offset: u64) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).context("seeking to the checkpoint")?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).context("reading past the checkpoint")?;
    Ok(buffer)
}

/// BLAKE3 of the 4 KB ending at `offset`. Empty when `offset` is 0, because
/// there is nothing before the start of a file to fingerprint.
pub fn prefix_hash(file: &mut File, offset: u64) -> Result<String> {
    if offset == 0 {
        return Ok(String::new());
    }
    let window = 4096u64.min(offset);
    file.seek(SeekFrom::Start(offset - window)).context("seeking to the prefix window")?;
    let mut buffer = vec![0u8; window as usize];
    file.read_exact(&mut buffer).context("reading the prefix window")?;
    Ok(blake3::hash(&buffer).to_hex().to_string())
}

/// Every `.jsonl` under `root`, sorted. Missing or unreadable directories are
/// skipped rather than failing the whole scan.
///
/// Symlinked transcripts are included — Claude Code links a subagent transcript
/// into a second session's folder, and both paths are indexed. Symlinked
/// *directories* are not followed, so a link cycle cannot spin the walk.
pub fn find_transcripts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "jsonl").unwrap_or(false) && path.is_file() {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}
