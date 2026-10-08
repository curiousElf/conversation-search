//! The tantivy side of a store: one document per turn.

use crate::parse::{Cli, Turn};
use crate::timestamps::to_unix_seconds;
use anyhow::{Context, Result};
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    DateOptions, FAST, Field, INDEXED, STORED, STRING, Schema, TEXT, Value as _,
};
use tantivy::{DateTime, Index, IndexReader, IndexWriter, Searcher, TantivyDocument, Term};

/// Handles for every field, so nothing looks fields up by name at query time.
#[derive(Clone, Copy)]
pub struct Fields {
    pub conversation_id: Field,
    pub turn_id: Field,
    pub cli: Field,
    pub project: Field,
    pub session_id: Field,
    pub source_path: Field,
    pub turn_number: Field,
    pub timestamp: Field,
    /// The timestamp exactly as the transcript wrote it. The DATE field above
    /// sorts and filters; this one is what results show, so nothing is lost to
    /// rounding.
    pub timestamp_text: Field,
    pub text: Field,
}

pub fn build_schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let date = DateOptions::from(INDEXED).set_stored().set_fast();
    let fields = Fields {
        conversation_id: builder.add_text_field("conversation_id", STRING | STORED),
        turn_id: builder.add_text_field("turn_id", STRING | STORED),
        cli: builder.add_text_field("cli", STRING | STORED),
        project: builder.add_text_field("project", STRING | STORED),
        session_id: builder.add_text_field("session_id", STRING | STORED),
        source_path: builder.add_text_field("source_path", STRING | STORED),
        turn_number: builder.add_u64_field("turn_number", FAST | STORED),
        timestamp: builder.add_date_field("timestamp", date),
        timestamp_text: builder.add_text_field("timestamp_text", STORED),
        text: builder.add_text_field("text", TEXT | STORED),
    };
    (builder.build(), fields)
}

/// Fingerprint of the schema, stored in `meta` so a binary can tell whether a
/// store's index was built with the field set it expects.
pub fn schema_fingerprint(schema: &Schema) -> String {
    let json = serde_json::to_vec(schema).expect("a tantivy schema always serialises");
    blake3::hash(&json).to_hex()[..16].to_string()
}

pub fn open_or_create(dir: &Path) -> Result<(Index, Fields)> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating index dir {}", dir.display()))?;
    let (schema, fields) = build_schema();
    let directory = tantivy::directory::MmapDirectory::open(dir)
        .with_context(|| format!("opening index dir {}", dir.display()))?;
    let index = Index::open_or_create(directory, schema)
        .with_context(|| format!("opening tantivy index at {}", dir.display()))?;
    Ok((index, fields))
}

pub fn reader(index: &Index) -> Result<IndexReader> {
    index
        .reader_builder()
        .reload_policy(tantivy::ReloadPolicy::Manual)
        .try_into()
        .context("building the tantivy reader")
}

pub fn document_for(fields: &Fields, turn: &Turn, project: &str, source_path: &str) -> TantivyDocument {
    let mut doc = TantivyDocument::default();
    doc.add_text(fields.conversation_id, &turn.conversation_id);
    doc.add_text(fields.turn_id, &turn.turn_id);
    doc.add_text(fields.cli, turn.cli.as_str());
    doc.add_text(fields.project, project);
    doc.add_text(fields.session_id, &turn.session_id);
    doc.add_text(fields.source_path, source_path);
    doc.add_u64(fields.turn_number, turn.turn_number);
    let seconds = to_unix_seconds(&turn.timestamp).unwrap_or(0);
    doc.add_date(fields.timestamp, DateTime::from_timestamp_secs(seconds));
    doc.add_text(fields.timestamp_text, &turn.timestamp);
    doc.add_text(fields.text, &turn.text);
    doc
}

pub fn delete_turn(writer: &IndexWriter, fields: &Fields, turn_id: &str) {
    writer.delete_term(Term::from_field_text(fields.turn_id, turn_id));
}

pub fn delete_source_path(writer: &IndexWriter, fields: &Fields, source_path: &str) {
    writer.delete_term(Term::from_field_text(fields.source_path, source_path));
}

/// One search result, already carrying everything a tool response needs.
pub struct Hit {
    pub turn_id: String,
    pub conversation_id: String,
    pub cli: String,
    pub session_id: String,
    pub project: String,
    pub source_path: String,
    pub turn_number: u64,
    pub timestamp: String,
    pub score: f32,
    pub text: String,
}

/// `cli` is `Some` for a single CLI and `None` for "all". `project` is an exact
/// match, empty meaning no filter.
pub fn search(
    searcher: &Searcher,
    fields: &Fields,
    query_text: &str,
    cli: Option<Cli>,
    project: &str,
    top_k: usize,
) -> Result<Vec<Hit>> {
    let parser = QueryParser::for_index(searcher.index(), vec![fields.text]);
    let (text_query, _errors) = parser.parse_query_lenient(query_text);

    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, text_query)];
    if let Some(cli) = cli {
        clauses.push((Occur::Must, exact(fields.cli, cli.as_str())));
    }
    if !project.is_empty() {
        clauses.push((Occur::Must, exact(fields.project, project)));
    }
    let query = BooleanQuery::new(clauses);

    let top = searcher
        .search(&query, &TopDocs::with_limit(top_k.max(1)).order_by_score())
        .context("running the search")?;

    let mut hits = Vec::with_capacity(top.len());
    for (score, address) in top {
        let doc: TantivyDocument = searcher.doc(address).context("loading a hit")?;
        hits.push(hit_from(fields, &doc, score));
    }
    Ok(hits)
}

pub fn turn_by_id(searcher: &Searcher, fields: &Fields, turn_id: &str) -> Result<Option<Hit>> {
    let query = exact(fields.turn_id, turn_id);
    let top = searcher.search(&query, &TopDocs::with_limit(1).order_by_score()).context("looking up a turn")?;
    let Some((score, address)) = top.into_iter().next() else {
        return Ok(None);
    };
    let doc: TantivyDocument = searcher.doc(address).context("loading the turn")?;
    Ok(Some(hit_from(fields, &doc, score)))
}

/// Every turn of one conversation, in turn order.
pub fn turns_in_conversation(
    searcher: &Searcher,
    fields: &Fields,
    conversation_id: &str,
) -> Result<Vec<Hit>> {
    let query = exact(fields.conversation_id, conversation_id);
    let top = searcher
        .search(&query, &TopDocs::with_limit(100_000).order_by_score())
        .context("loading a conversation")?;
    let mut hits = Vec::with_capacity(top.len());
    for (score, address) in top {
        let doc: TantivyDocument = searcher.doc(address).context("loading a turn")?;
        hits.push(hit_from(fields, &doc, score));
    }
    hits.sort_by_key(|hit| hit.turn_number);
    Ok(hits)
}

fn exact(field: Field, value: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        Term::from_field_text(field, value),
        tantivy::schema::IndexRecordOption::Basic,
    ))
}

fn hit_from(fields: &Fields, doc: &TantivyDocument, score: f32) -> Hit {
    let text_of = |field: Field| -> String {
        doc.get_first(field).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    Hit {
        turn_id: text_of(fields.turn_id),
        conversation_id: text_of(fields.conversation_id),
        cli: text_of(fields.cli),
        session_id: text_of(fields.session_id),
        project: text_of(fields.project),
        source_path: text_of(fields.source_path),
        turn_number: doc.get_first(fields.turn_number).and_then(|v| v.as_u64()).unwrap_or(0),
        timestamp: text_of(fields.timestamp_text),
        score,
        text: text_of(fields.text),
    }
}
