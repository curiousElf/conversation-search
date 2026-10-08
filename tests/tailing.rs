//! Checks over the tailing logic that run without a live CLI session.
//!
//! Each test drives the same code the daemon's ingestion thread runs —
//! `process_file` followed by a commit and a SQLite write — against a private
//! copy of the fixtures in a temp directory. Dropping and reopening the store
//! stands in for a SIGKILL: nothing is carried in memory between the two.

use conversation_search::ingest::process_file;
use conversation_search::parse::Cli;
use conversation_search::store::{self, index, meta};
use conversation_search::watch;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A scratch directory that cleans itself up.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "conversation-search-test-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("creating the scratch directory");
        Scratch { dir }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // The fixture copy is made read-only, so restore write permission first.
        let _ = std::process::Command::new("/bin/chmod").args(["-R", "u+w"]).arg(&self.dir).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture_bytes(relative: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(relative))
        .expect("reading a fixture")
}

/// Runs one ingestion pass over `paths`, exactly as the daemon does: process,
/// commit, then write the checkpoints the commit covers.
fn ingest_once(store_dir: &Path, paths: &[(PathBuf, Cli)]) {
    let mut store = store::open(store_dir).expect("opening the store");
    let mut writer = store.index.writer(16 * 1024 * 1024).expect("writer");
    let mut updates = Vec::new();
    for (path, cli) in paths {
        match process_file(&store.db, &writer, &store.fields, path, *cli) {
            Ok(Some(update)) => updates.push(update),
            Ok(None) => {}
            Err(error) => panic!("processing {}: {error:#}", path.display()),
        }
    }
    writer.commit().expect("commit");
    let tx = store.db.transaction().expect("transaction");
    for update in &updates {
        meta::upsert_file(&tx, &update.file).expect("file row");
        meta::upsert_session(&tx, &update.session).expect("session row");
    }
    meta::bump_generation(&tx).expect("generation");
    tx.commit().expect("commit rows");
}

/// Every indexed turn as `turn_id -> hash of its text`. Two stores that produce
/// the same map hold the same turns with the same content.
fn turn_fingerprints(store_dir: &Path) -> BTreeMap<String, String> {
    let store = store::open(store_dir).expect("opening the store");
    let reader = index::reader(&store.index).expect("reader");
    let searcher = reader.searcher();

    let mut all = BTreeMap::new();
    let mut duplicates = Vec::new();
    for conversation in conversation_ids(&store) {
        for hit in index::turns_in_conversation(&searcher, &store.fields, &conversation)
            .expect("reading a conversation")
        {
            let hash = blake3::hash(hit.text.as_bytes()).to_hex().to_string();
            if all.insert(hit.turn_id.clone(), hash).is_some() {
                duplicates.push(hit.turn_id);
            }
        }
    }
    assert!(duplicates.is_empty(), "duplicate turn ids in the index: {duplicates:?}");
    all
}

fn conversation_ids(store: &store::Store) -> Vec<String> {
    let mut statement =
        store.db.prepare("SELECT conversation_id FROM sessions").expect("preparing");
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("querying")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collecting");
    ids
}

fn checkpoint_of(store_dir: &Path, path: &Path) -> u64 {
    let store = store::open(store_dir).expect("opening the store");
    meta::get_file(&store.db, &path.to_string_lossy())
        .expect("reading the file row")
        .expect("the file has a row")
        .checkpoint
}

#[test]
fn appending_in_tiny_chunks_never_checkpoints_a_partial_line() {
    let scratch = Scratch::new("chunks");
    let transcript = scratch.dir.join("session.jsonl");
    let store_dir = scratch.dir.join("store");

    let whole = fixture_bytes("codex/2026/01/04/rollout-2026-01-04T10-00-00-0199e790-c9b9-7f41-b509-09f9694a0da2.jsonl");
    // Start from the first two records so the file is already a live transcript.
    let split = whole.iter().enumerate().filter(|(_, b)| **b == b'\n').nth(1).unwrap().0 + 1;
    std::fs::write(&transcript, &whole[..split]).expect("seeding the transcript");

    let paths = vec![(transcript.clone(), Cli::Codex)];
    ingest_once(&store_dir, &paths);

    // Append the rest in 1-to-7 byte chunks, including splits inside multibyte
    // UTF-8, re-opening the store between every chunk.
    let mut written = split;
    let mut chunk = 1usize;
    while written < whole.len() {
        let end = (written + chunk).min(whole.len());
        let mut file =
            std::fs::OpenOptions::new().append(true).open(&transcript).expect("appending");
        file.write_all(&whole[written..end]).expect("writing a chunk");
        drop(file);
        written = end;
        chunk = if chunk == 7 { 1 } else { chunk + 1 };

        ingest_once(&store_dir, &paths);

        let checkpoint = checkpoint_of(&store_dir, &transcript) as usize;
        let consumed = &whole[..written];
        let last_newline =
            consumed.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
        assert!(
            checkpoint <= last_newline,
            "checkpoint {checkpoint} ran past the last complete line at {last_newline}"
        );
    }

    // The result must equal a store built in one pass from the finished file.
    let clean_dir = scratch.dir.join("clean");
    ingest_once(&clean_dir, &paths);
    assert_eq!(turn_fingerprints(&store_dir), turn_fingerprints(&clean_dir));
}

#[test]
fn same_size_rewrite_is_detected_and_the_file_is_reindexed() {
    let scratch = Scratch::new("rewrite");
    let transcript = scratch.dir.join("session.jsonl");
    let store_dir = scratch.dir.join("store");

    let whole = fixture_bytes("codex/2026/01/04/rollout-2026-01-04T10-00-00-0199e790-c9b9-7f41-b509-09f9694a0da2.jsonl");
    std::fs::write(&transcript, &whole).expect("seeding the transcript");
    let paths = vec![(transcript.clone(), Cli::Codex)];
    ingest_once(&store_dir, &paths);
    let before = turn_fingerprints(&store_dir);
    assert!(!before.is_empty());

    // Same length, different bytes: swap one word inside the first user message.
    let text = String::from_utf8(whole.clone()).expect("fixture is UTF-8");
    let rewritten = text.replacen("duplicate manifest entry", "duplicate manifest ENTRY", 1);
    assert_eq!(rewritten.len(), text.len(), "the rewrite must not change the size");
    assert_ne!(rewritten, text);
    std::fs::write(&transcript, rewritten.as_bytes()).expect("rewriting");

    ingest_once(&store_dir, &paths);
    let after = turn_fingerprints(&store_dir);

    assert_eq!(after.len(), before.len(), "a rewrite must not duplicate turns");
    assert_ne!(after, before, "the rewritten text was not picked up");

    let clean_dir = scratch.dir.join("clean");
    ingest_once(&clean_dir, &paths);
    assert_eq!(after, turn_fingerprints(&clean_dir));
}

#[test]
fn a_deleted_transcript_loses_its_documents_and_rows() {
    let scratch = Scratch::new("delete");
    let roots = watch::Roots {
        claude: scratch.dir.join("claude"),
        codex: scratch.dir.join("codex"),
    };
    std::fs::create_dir_all(&roots.codex).expect("creating the codex root");
    let transcript = roots.codex.join("rollout-2026-01-04T10-00-00-0199e790-c9b9-7f41-b509-09f9694a0da2.jsonl");
    std::fs::write(
        &transcript,
        fixture_bytes("codex/2026/01/04/rollout-2026-01-04T10-00-00-0199e790-c9b9-7f41-b509-09f9694a0da2.jsonl"),
    )
    .expect("seeding");

    let store_dir = scratch.dir.join("store");
    ingest_once(&store_dir, &[(transcript.clone(), Cli::Codex)]);
    assert!(!turn_fingerprints(&store_dir).is_empty());

    std::fs::remove_file(&transcript).expect("deleting the transcript");

    let mut store = store::open(&store_dir).expect("opening the store");
    let found = watch::reconcile(&store.db, &roots.claude, &roots.codex).expect("reconcile");
    assert_eq!(found.vanished.len(), 1, "the deleted file was not noticed");

    let mut writer = store.index.writer(16 * 1024 * 1024).expect("writer");
    let tx = store.db.transaction().expect("transaction");
    for path in &found.vanished {
        conversation_search::ingest::forget_file(&tx, &writer, &store.fields, path)
            .expect("forgetting");
    }
    tx.commit().expect("commit rows");
    writer.commit().expect("commit index");
    drop(writer);
    drop(store);

    assert!(turn_fingerprints(&store_dir).is_empty(), "documents survived the deletion");
    let store = store::open(&store_dir).expect("reopening");
    assert!(meta::all_files(&store.db).expect("files").is_empty(), "the file row survived");
}

#[test]
fn transcripts_are_never_written_to() {
    let scratch = Scratch::new("readonly");
    let root = scratch.dir.join("codex");
    std::fs::create_dir_all(&root).expect("creating the root");
    let transcript = root.join("rollout-2026-01-05T08-00-00-0199f001-1111-7222-8333-444455556666.jsonl");
    std::fs::write(
        &transcript,
        fixture_bytes("codex/2026/01/05/rollout-2026-01-05T08-00-00-0199f001-1111-7222-8333-444455556666.jsonl"),
    )
    .expect("seeding");

    // Everything under the transcript root loses write permission, so any
    // attempt to open one for writing fails.
    let chmod = std::process::Command::new("/bin/chmod")
        .args(["-R", "a-w"])
        .arg(&root)
        .output()
        .expect("chmod");
    assert!(chmod.status.success());

    let store_dir = scratch.dir.join("store");
    ingest_once(&store_dir, &[(transcript.clone(), Cli::Codex)]);
    assert!(!turn_fingerprints(&store_dir).is_empty(), "a read-only transcript must still index");

    assert!(
        std::fs::OpenOptions::new().write(true).open(&transcript).is_err(),
        "the fixture root is not actually read-only, so this test proves nothing"
    );
}

/// Processes and commits, then drops the store without writing the checkpoints —
/// the crash window between step (1) and step (2) of the publish sequence.
fn ingest_and_crash_after_commit(store_dir: &Path, paths: &[(PathBuf, Cli)]) {
    let store = store::open(store_dir).expect("opening the store");
    let mut writer = store.index.writer(16 * 1024 * 1024).expect("writer");
    for (path, cli) in paths {
        process_file(&store.db, &writer, &store.fields, path, *cli).expect("processing");
    }
    writer.commit().expect("commit");
}

/// Processes without committing at all — the crash window during the read.
fn ingest_and_crash_before_commit(store_dir: &Path, paths: &[(PathBuf, Cli)]) {
    let store = store::open(store_dir).expect("opening the store");
    let writer = store.index.writer(16 * 1024 * 1024).expect("writer");
    for (path, cli) in paths {
        process_file(&store.db, &writer, &store.fields, path, *cli).expect("processing");
    }
}

#[test]
fn a_crash_between_the_commit_and_the_checkpoint_does_not_duplicate_turns() {
    let scratch = Scratch::new("crash-after-commit");
    let transcript = scratch.dir.join("session.jsonl");
    let store_dir = scratch.dir.join("store");
    let whole = fixture_bytes("codex/2026/01/04/rollout-2026-01-04T10-00-00-0199e790-c9b9-7f41-b509-09f9694a0da2.jsonl");
    std::fs::write(&transcript, &whole).expect("seeding");
    let paths = vec![(transcript.clone(), Cli::Codex)];

    ingest_and_crash_after_commit(&store_dir, &paths);
    // Restart: the checkpoint is still 0, so the same bytes are re-read and the
    // same delete-and-add is re-issued.
    ingest_once(&store_dir, &paths);

    let clean_dir = scratch.dir.join("clean");
    ingest_once(&clean_dir, &paths);
    assert_eq!(turn_fingerprints(&store_dir), turn_fingerprints(&clean_dir));
}

#[test]
fn a_crash_before_the_commit_loses_nothing() {
    let scratch = Scratch::new("crash-before-commit");
    let transcript = scratch.dir.join("session.jsonl");
    let store_dir = scratch.dir.join("store");
    let whole = fixture_bytes("codex/2026/01/04/rollout-2026-01-04T10-00-00-0199e790-c9b9-7f41-b509-09f9694a0da2.jsonl");
    std::fs::write(&transcript, &whole).expect("seeding");
    let paths = vec![(transcript.clone(), Cli::Codex)];

    ingest_and_crash_before_commit(&store_dir, &paths);
    ingest_once(&store_dir, &paths);

    let clean_dir = scratch.dir.join("clean");
    ingest_once(&clean_dir, &paths);
    assert_eq!(turn_fingerprints(&store_dir), turn_fingerprints(&clean_dir));
}
