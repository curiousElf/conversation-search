//! The SQLite side of a store: sessions, per-file checkpoints, and the keys
//! that say which binary built the store.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

/// Bumped whenever the SQLite or tantivy layout changes. A store built by an
/// older version is migrated into a new numbered store; a store built by a newer
/// one makes the daemon refuse to start.
pub const SCHEMA_VERSION: i64 = 1;

pub const KEY_SCHEMA_VERSION: &str = "schema_version";
pub const KEY_TANTIVY_SCHEMA_HASH: &str = "tantivy_schema_hash";
pub const KEY_BINARY_VERSION: &str = "binary_version";
pub const KEY_GENERATION: &str = "generation";

#[derive(Clone, Debug)]
pub struct FileRow {
    pub path: String,
    pub cli: String,
    pub conversation_id: String,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
    pub checkpoint: u64,
    pub checkpoint_turn_number: u64,
    pub prefix_hash: String,
    pub last_indexed_at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct SessionRow {
    pub conversation_id: String,
    pub cli: String,
    pub session_id: String,
    pub source_path: String,
    pub project: String,
    pub cwd: String,
    pub git_branch: String,
    pub slug: String,
    pub first_ts: String,
    pub last_ts: String,
    pub turn_count: u64,
}

pub fn open(db_path: &Path) -> Result<Connection> {
    let connection = Connection::open(db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .context("enabling WAL")?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .context("setting synchronous")?;
    create_tables(&connection)?;
    Ok(connection)
}

fn create_tables(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS files (
                 path TEXT PRIMARY KEY,
                 cli TEXT NOT NULL,
                 conversation_id TEXT NOT NULL,
                 dev INTEGER NOT NULL,
                 ino INTEGER NOT NULL,
                 size INTEGER NOT NULL,
                 mtime INTEGER NOT NULL,
                 checkpoint INTEGER NOT NULL,
                 checkpoint_turn_number INTEGER NOT NULL,
                 prefix_hash TEXT NOT NULL,
                 last_indexed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 conversation_id TEXT PRIMARY KEY,
                 cli TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 source_path TEXT NOT NULL,
                 project TEXT NOT NULL,
                 cwd TEXT NOT NULL,
                 git_branch TEXT NOT NULL,
                 slug TEXT NOT NULL,
                 first_ts TEXT NOT NULL,
                 last_ts TEXT NOT NULL,
                 turn_count INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS sessions_by_last_ts ON sessions (cli, last_ts DESC);",
        )
        .context("creating the store tables")?;
    Ok(())
}

pub fn get_meta(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |row| row.get(0))
        .optional()
        .with_context(|| format!("reading meta key {key}"))
}

pub fn set_meta(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .with_context(|| format!("writing meta key {key}"))?;
    Ok(())
}

pub fn generation(connection: &Connection) -> Result<u64> {
    Ok(get_meta(connection, KEY_GENERATION)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

pub fn bump_generation(tx: &Transaction) -> Result<u64> {
    let current: u64 = tx
        .query_row("SELECT value FROM meta WHERE key = ?1", params![KEY_GENERATION], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .context("reading the generation")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let next = current + 1;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![KEY_GENERATION, next.to_string()],
    )
    .context("bumping the generation")?;
    Ok(next)
}

fn file_from_row(row: &rusqlite::Row) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        path: row.get(0)?,
        cli: row.get(1)?,
        conversation_id: row.get(2)?,
        dev: row.get::<_, i64>(3)? as u64,
        ino: row.get::<_, i64>(4)? as u64,
        size: row.get::<_, i64>(5)? as u64,
        mtime: row.get(6)?,
        checkpoint: row.get::<_, i64>(7)? as u64,
        checkpoint_turn_number: row.get::<_, i64>(8)? as u64,
        prefix_hash: row.get(9)?,
        last_indexed_at: row.get(10)?,
    })
}

const FILE_COLUMNS: &str = "path, cli, conversation_id, dev, ino, size, mtime, checkpoint, \
     checkpoint_turn_number, prefix_hash, last_indexed_at";

pub fn get_file(connection: &Connection, path: &str) -> Result<Option<FileRow>> {
    let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE path = ?1");
    connection
        .query_row(&sql, params![path], file_from_row)
        .optional()
        .with_context(|| format!("reading file row for {path}"))
}

pub fn all_files(connection: &Connection) -> Result<Vec<FileRow>> {
    let sql = format!("SELECT {FILE_COLUMNS} FROM files");
    let mut statement = connection.prepare(&sql).context("preparing the file scan")?;
    let rows = statement
        .query_map([], file_from_row)
        .context("scanning files")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collecting files")?;
    Ok(rows)
}

pub fn upsert_file(tx: &Transaction, file: &FileRow) -> Result<()> {
    tx.execute(
        "INSERT INTO files (path, cli, conversation_id, dev, ino, size, mtime, checkpoint,
                            checkpoint_turn_number, prefix_hash, last_indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(path) DO UPDATE SET
             cli = excluded.cli,
             conversation_id = excluded.conversation_id,
             dev = excluded.dev,
             ino = excluded.ino,
             size = excluded.size,
             mtime = excluded.mtime,
             checkpoint = excluded.checkpoint,
             checkpoint_turn_number = excluded.checkpoint_turn_number,
             prefix_hash = excluded.prefix_hash,
             last_indexed_at = excluded.last_indexed_at",
        params![
            file.path,
            file.cli,
            file.conversation_id,
            file.dev as i64,
            file.ino as i64,
            file.size as i64,
            file.mtime,
            file.checkpoint as i64,
            file.checkpoint_turn_number as i64,
            file.prefix_hash,
            file.last_indexed_at,
        ],
    )
    .with_context(|| format!("writing file row for {}", file.path))?;
    Ok(())
}

pub fn delete_file(tx: &Transaction, path: &str) -> Result<()> {
    tx.execute("DELETE FROM files WHERE path = ?1", params![path])
        .with_context(|| format!("deleting file row for {path}"))?;
    tx.execute("DELETE FROM sessions WHERE source_path = ?1", params![path])
        .with_context(|| format!("deleting session row for {path}"))?;
    Ok(())
}

pub fn upsert_session(tx: &Transaction, session: &SessionRow) -> Result<()> {
    tx.execute(
        "INSERT INTO sessions (conversation_id, cli, session_id, source_path, project, cwd,
                               git_branch, slug, first_ts, last_ts, turn_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(conversation_id) DO UPDATE SET
             cli = excluded.cli,
             session_id = excluded.session_id,
             source_path = excluded.source_path,
             project = excluded.project,
             cwd = excluded.cwd,
             git_branch = excluded.git_branch,
             slug = excluded.slug,
             first_ts = excluded.first_ts,
             last_ts = excluded.last_ts,
             turn_count = excluded.turn_count",
        params![
            session.conversation_id,
            session.cli,
            session.session_id,
            session.source_path,
            session.project,
            session.cwd,
            session.git_branch,
            session.slug,
            session.first_ts,
            session.last_ts,
            session.turn_count as i64,
        ],
    )
    .with_context(|| format!("writing session row for {}", session.conversation_id))?;
    Ok(())
}

fn session_from_row(row: &rusqlite::Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        conversation_id: row.get(0)?,
        cli: row.get(1)?,
        session_id: row.get(2)?,
        source_path: row.get(3)?,
        project: row.get(4)?,
        cwd: row.get(5)?,
        git_branch: row.get(6)?,
        slug: row.get(7)?,
        first_ts: row.get(8)?,
        last_ts: row.get(9)?,
        turn_count: row.get::<_, i64>(10)? as u64,
    })
}

const SESSION_COLUMNS: &str = "conversation_id, cli, session_id, source_path, project, cwd, \
     git_branch, slug, first_ts, last_ts, turn_count";

/// Sessions newest first. `cli` empty means every CLI; `project` empty means no
/// project filter. Returns the page plus the unpaged total.
pub fn list_sessions(
    connection: &Connection,
    cli: &str,
    project: &str,
    limit: u64,
    offset: u64,
) -> Result<(Vec<SessionRow>, u64)> {
    let where_clause = "WHERE (?1 = '' OR cli = ?1) AND (?2 = '' OR project = ?2)";
    let total: u64 = connection
        .query_row(
            &format!("SELECT COUNT(*) FROM sessions {where_clause}"),
            params![cli, project],
            |row| row.get::<_, i64>(0),
        )
        .context("counting sessions")? as u64;

    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions {where_clause}
         ORDER BY last_ts DESC LIMIT ?3 OFFSET ?4"
    );
    let mut statement = connection.prepare(&sql).context("preparing the session list")?;
    let rows = statement
        .query_map(params![cli, project, limit as i64, offset as i64], session_from_row)
        .context("listing sessions")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collecting sessions")?;
    Ok((rows, total))
}

pub fn session_by_conversation(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Option<SessionRow>> {
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE conversation_id = ?1");
    connection
        .query_row(&sql, params![conversation_id], session_from_row)
        .optional()
        .context("reading a session")
}

/// Session and turn totals per CLI, for `status` and `index_status`.
pub struct Counts {
    pub cli: String,
    pub sessions: u64,
    pub turns: u64,
}

pub fn counts_by_cli(connection: &Connection) -> Result<Vec<Counts>> {
    let mut statement = connection
        .prepare(
            "SELECT cli, COUNT(*), COALESCE(SUM(turn_count), 0) FROM sessions GROUP BY cli ORDER BY cli",
        )
        .context("preparing the counts query")?;
    let rows = statement
        .query_map([], |row| {
            Ok(Counts {
                cli: row.get(0)?,
                sessions: row.get::<_, i64>(1)? as u64,
                turns: row.get::<_, i64>(2)? as u64,
            })
        })
        .context("counting")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collecting counts")?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        create_tables(&connection).unwrap();
        connection
    }

    #[test]
    fn generation_starts_at_zero_and_increments() {
        let mut connection = in_memory();
        assert_eq!(generation(&connection).unwrap(), 0);
        let tx = connection.transaction().unwrap();
        assert_eq!(bump_generation(&tx).unwrap(), 1);
        assert_eq!(bump_generation(&tx).unwrap(), 2);
        tx.commit().unwrap();
        assert_eq!(generation(&connection).unwrap(), 2);
    }

    #[test]
    fn deleting_a_file_also_drops_its_session() {
        let mut connection = in_memory();
        let tx = connection.transaction().unwrap();
        upsert_session(
            &tx,
            &SessionRow {
                conversation_id: "c1".into(),
                cli: "codex".into(),
                source_path: "/tmp/a.jsonl".into(),
                turn_count: 3,
                ..Default::default()
            },
        )
        .unwrap();
        delete_file(&tx, "/tmp/a.jsonl").unwrap();
        tx.commit().unwrap();
        assert!(session_by_conversation(&connection, "c1").unwrap().is_none());
    }
}
