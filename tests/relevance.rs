//! The relevance fixture set: ten queries whose answer must be in the top five.
//!
//! Ignored by default because it searches the live store on this machine.
//!
//!     cargo test --test relevance -- --ignored --nocapture

use conversation_search::parse::Cli;
use conversation_search::store::{self, index};

#[derive(serde::Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    query: String,
    cli: String,
    expect_conversation_id: String,
    about: String,
}

const TOP_K: usize = 5;

#[test]
#[ignore = "searches the live store on this machine"]
fn every_relevance_case_lands_in_the_top_five() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/relevance.json"),
    )
    .expect("reading the relevance fixtures");
    let cases: Cases = serde_json::from_str(&text).expect("relevance fixtures are valid JSON");

    let store = store::open_current().expect("opening the current store");
    let reader = index::reader(&store.index).expect("building a reader");
    let searcher = reader.searcher();

    let mut failures = Vec::new();
    for case in &cases.cases {
        let cli = Cli::from_str(&case.cli);
        assert!(cli.is_some(), "case {:?} names an unknown cli", case.query);
        let hits = index::search(&searcher, &store.fields, &case.query, cli, "", TOP_K)
            .expect("searching");
        let found = hits
            .iter()
            .position(|hit| hit.conversation_id == case.expect_conversation_id);
        match found {
            Some(rank) => println!("ok   rank {}  {} — {}", rank + 1, case.query, case.about),
            None => {
                println!("FAIL       {} — expected {}", case.query, case.about);
                for (rank, hit) in hits.iter().enumerate() {
                    println!("       {}. {} {}", rank + 1, hit.conversation_id, hit.project);
                }
                failures.push(case.query.clone());
            }
        }
    }
    assert!(failures.is_empty(), "relevance cases outside the top {TOP_K}: {failures:?}");
}
