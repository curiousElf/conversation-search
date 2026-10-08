//! After any restart the index must equal what a clean rebuild would produce.
//!
//! Ignored by default: it reads the machine's transcripts and the live store.
//!
//!     cargo test --release --test rebuild_equivalence -- --ignored --nocapture
//!
//! Transcripts of sessions that are open right now keep growing while the
//! comparison runs, so a conversation whose file changed during the run is
//! reported and excluded. Everything else must match exactly.

use conversation_search::reindex;
use conversation_search::store::{self, index};
use conversation_search::watch;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `turn_id -> (source path, hash of the turn text)`, asserting no turn id
/// appears twice.
fn fingerprints(store_dir: &Path) -> BTreeMap<String, (String, String)> {
    let store = store::open(store_dir).expect("opening the store");
    let reader = index::reader(&store.index).expect("reader");
    let searcher = reader.searcher();

    let mut statement =
        store.db.prepare("SELECT conversation_id FROM sessions").expect("preparing");
    let conversations: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("querying")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collecting");

    let mut all = BTreeMap::new();
    let mut duplicates = Vec::new();
    for conversation in conversations {
        for hit in index::turns_in_conversation(&searcher, &store.fields, &conversation)
            .expect("reading a conversation")
        {
            let hash = blake3::hash(hit.text.as_bytes()).to_hex().to_string();
            if all.insert(hit.turn_id.clone(), (hit.source_path, hash)).is_some() {
                duplicates.push(hit.turn_id);
            }
        }
    }
    assert!(duplicates.is_empty(), "duplicate turn ids in {}: {duplicates:?}", store_dir.display());
    all
}

fn modified_since(path: &str, since: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|modified| modified > since)
        .unwrap_or(true)
}

#[test]
#[ignore = "reads the machine's transcripts and the live store"]
fn the_live_store_equals_a_clean_rebuild() {
    let started = SystemTime::now();
    let live_dir = store::current_store_dir()
        .expect("reading the current-store symlink")
        .expect("a store is current");
    let live = fingerprints(&live_dir);
    println!("live store {} holds {} turns", live_dir.display(), live.len());

    let scratch: PathBuf = std::env::temp_dir().join(format!(
        "conversation-search-rebuild-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch).expect("creating the scratch store");
    let roots = watch::roots_from_home().expect("roots");
    let progress = reindex::Progress::default();
    let summary = reindex::build_store(&scratch, &roots, &progress).expect("clean rebuild");
    println!("clean rebuild: {} turns from {} files", summary.turns, summary.files);
    let clean = fingerprints(&scratch);

    let mut mismatched = Vec::new();
    let mut excluded = 0usize;
    for (turn_id, (source, hash)) in &clean {
        match live.get(turn_id) {
            Some((_, live_hash)) if live_hash == hash => {}
            _ => {
                if modified_since(source, started) {
                    excluded += 1;
                } else {
                    mismatched.push((turn_id.clone(), source.clone()));
                }
            }
        }
    }
    for (turn_id, (source, _)) in &live {
        if !clean.contains_key(turn_id) {
            if modified_since(source, started) {
                excluded += 1;
            } else {
                mismatched.push((turn_id.clone(), source.clone()));
            }
        }
    }

    std::fs::remove_dir_all(&scratch).ok();

    println!(
        "{} turns compared, {} excluded because their transcript changed during the run, {} mismatched",
        clean.len(),
        excluded,
        mismatched.len()
    );
    for (turn_id, source) in mismatched.iter().take(10) {
        println!("  MISMATCH {turn_id} from {source}");
    }
    assert!(mismatched.is_empty(), "the live store differs from a clean rebuild");
}
