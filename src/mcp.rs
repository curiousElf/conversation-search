//! The MCP tools and the HTTP endpoint that serves them.
//!
//! Tool names are stable. Every content tool takes a required `cli`
//! argument — `claude_code`, `codex`, or `all` — so a caller always says which
//! corpus it means, and every result carries the `cli` it came from.

use crate::daemon::Shared;
use crate::parse::Cli;
use crate::paths;
use crate::agent::Health;
use crate::store::{index, meta};
use anyhow::{Context, Result};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    #[schemars(description = "Search keywords, for example \"docker build failing\"")]
    pub query: String,
    #[schemars(description = "Which corpus to search: claude_code, codex, or all. Required.")]
    pub cli: String,
    #[schemars(description = "Optional exact project filter")]
    #[serde(default)]
    pub project: String,
    #[schemars(description = "Maximum results, default 10")]
    #[serde(default)]
    pub top_k: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    #[schemars(description = "Which corpus to list: claude_code, codex, or all. Required.")]
    pub cli: String,
    #[schemars(description = "Optional exact project filter")]
    #[serde(default)]
    pub project: String,
    #[schemars(description = "Page size, default 50")]
    #[serde(default)]
    pub limit: Option<u64>,
    #[schemars(description = "Skip this many sessions, default 0")]
    #[serde(default)]
    pub offset: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadTurnArgs {
    #[schemars(description = "Which corpus the turn came from: claude_code, codex, or all. Required.")]
    pub cli: String,
    #[schemars(description = "Opaque turn id from a search or list result")]
    pub turn_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadConversationArgs {
    #[schemars(description = "Which corpus the conversation came from: claude_code, codex, or all. Required.")]
    pub cli: String,
    #[schemars(description = "Opaque conversation id from a search or list result")]
    pub conversation_id: String,
    #[schemars(description = "First turn to return, default 0")]
    #[serde(default)]
    pub offset: Option<usize>,
    #[schemars(description = "Number of turns to return, default 20")]
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NoArgs {}

#[derive(Clone)]
pub struct Tools {
    shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

impl Tools {
    pub fn new(shared: Arc<Shared>) -> Tools {
        Tools { shared, tool_router: Self::tool_router() }
    }
}

/// `all` means no filter. Anything else must name one of the two CLIs; omitting
/// the argument is a schema error before it reaches here.
fn cli_filter(name: &str) -> Result<Option<Cli>, ErrorData> {
    match name {
        "all" => Ok(None),
        other => Cli::from_str(other).map(Some).ok_or_else(|| {
            ErrorData::invalid_params(
                format!("cli must be claude_code, codex, or all (got {other:?})"),
                None,
            )
        }),
    }
}

/// A failed call the client can recognise as failed (`isError`), with the reason as text.
fn tool_error(message: String) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![ContentBlock::text(message)]))
}

fn json_result(value: serde_json::Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(value.to_string())]))
}

fn internal(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

#[tool_router]
impl Tools {
    #[tool(description = "BM25 keyword search across past Claude Code and Codex conversation turns. \
                          The `cli` argument is required: claude_code, codex, or all.")]
    fn search_conversations(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let filter = cli_filter(&args.cli)?;
        let top_k = args.top_k.unwrap_or(10);
        let published = self.shared.published.lock().expect("publication lock");
        let searcher = published.reader.searcher();
        let hits = index::search(
            &searcher,
            &self.shared.fields,
            &args.query,
            filter,
            &args.project,
            top_k,
        )
        .map_err(internal)?;

        let results: Vec<serde_json::Value> = hits
            .iter()
            .map(|hit| {
                serde_json::json!({
                    "turn_id": hit.turn_id,
                    "conversation_id": hit.conversation_id,
                    "cli": hit.cli,
                    "session_id": hit.session_id,
                    "project": hit.project,
                    "turn_number": hit.turn_number,
                    "timestamp": hit.timestamp,
                    "score": (hit.score * 10_000.0).round() / 10_000.0,
                    "snippet": hit.text.chars().take(300).collect::<String>(),
                })
            })
            .collect();
        json_result(serde_json::json!({
            "query": args.query,
            "cli": args.cli,
            "total": results.len(),
            "results": results,
        }))
    }

    #[tool(description = "Browse indexed conversations, newest first. \
                          The `cli` argument is required: claude_code, codex, or all.")]
    fn list_conversations(
        &self,
        Parameters(args): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let filter = cli_filter(&args.cli)?;
        let cli_column = filter.map(|c| c.as_str()).unwrap_or("");
        let published = self.shared.published.lock().expect("publication lock");
        let (rows, total) = meta::list_sessions(
            &published.db,
            cli_column,
            &args.project,
            args.limit.unwrap_or(50),
            args.offset.unwrap_or(0),
        )
        .map_err(internal)?;

        let conversations: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "conversation_id": row.conversation_id,
                    "cli": row.cli,
                    "session_id": row.session_id,
                    "project": row.project,
                    "cwd": row.cwd,
                    "git_branch": row.git_branch,
                    "slug": row.slug,
                    "first_timestamp": row.first_ts,
                    "last_timestamp": row.last_ts,
                    "turn_count": row.turn_count,
                })
            })
            .collect();
        json_result(serde_json::json!({ "conversations": conversations, "total": total }))
    }

    #[tool(description = "Read the full text of one conversation turn. \
                          The `cli` argument is required and must match the turn's own cli.")]
    fn read_turn(
        &self,
        Parameters(args): Parameters<ReadTurnArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let filter = cli_filter(&args.cli)?;
        let published = self.shared.published.lock().expect("publication lock");
        let searcher = published.reader.searcher();
        let found = index::turn_by_id(&searcher, &self.shared.fields, &args.turn_id)
            .map_err(internal)?;

        let Some(hit) = found else {
            return tool_error(format!("turn {:?} not found", args.turn_id));
        };
        if let Some(wanted) = filter {
            if hit.cli != wanted.as_str() {
                return tool_error(format!(
                        "turn {:?} belongs to cli {:?}, not {:?}",
                        args.turn_id, hit.cli, args.cli
                    ));
            }
        }
        json_result(serde_json::json!({
            "turn_id": hit.turn_id,
            "conversation_id": hit.conversation_id,
            "cli": hit.cli,
            "session_id": hit.session_id,
            "project": hit.project,
            "turn_number": hit.turn_number,
            "timestamp": hit.timestamp,
            "text": hit.text,
        }))
    }

    #[tool(description = "Paginate through the turns of one conversation. \
                          The `cli` argument is required and must match the conversation's own cli.")]
    fn read_conversation(
        &self,
        Parameters(args): Parameters<ReadConversationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let filter = cli_filter(&args.cli)?;
        let offset = args.offset.unwrap_or(0);
        let limit = args.limit.unwrap_or(20);
        let published = self.shared.published.lock().expect("publication lock");
        let session = meta::session_by_conversation(&published.db, &args.conversation_id)
            .map_err(internal)?;
        let Some(session) = session else {
            return tool_error(format!("conversation {:?} not found", args.conversation_id));
        };
        if let Some(wanted) = filter {
            if session.cli != wanted.as_str() {
                return tool_error(format!(
                        "conversation {:?} belongs to cli {:?}, not {:?}",
                        args.conversation_id, session.cli, args.cli
                    ));
            }
        }

        let searcher = published.reader.searcher();
        let all = index::turns_in_conversation(&searcher, &self.shared.fields, &args.conversation_id)
            .map_err(internal)?;
        let page: Vec<serde_json::Value> = all
            .iter()
            .skip(offset)
            .take(limit)
            .map(|hit| {
                serde_json::json!({
                    "turn_id": hit.turn_id,
                    "turn_number": hit.turn_number,
                    "timestamp": hit.timestamp,
                    "text": hit.text,
                })
            })
            .collect();

        json_result(serde_json::json!({
            "conversation_id": session.conversation_id,
            "cli": session.cli,
            "session_id": session.session_id,
            "project": session.project,
            "cwd": session.cwd,
            "git_branch": session.git_branch,
            "slug": session.slug,
            "first_timestamp": session.first_ts,
            "last_timestamp": session.last_ts,
            "total_turns": all.len(),
            "offset": offset,
            "limit": limit,
            "turns": page,
        }))
    }

    #[tool(description = "Index counts, published generation, and daemon health. \
                          The one tool that takes no cli argument.")]
    fn index_status(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let health = health_of(&self.shared);
        let published = self.shared.published.lock().expect("publication lock");
        let counts = meta::counts_by_cli(&published.db).map_err(internal)?;
        let per_cli: Vec<serde_json::Value> = counts
            .iter()
            .map(|c| {
                serde_json::json!({ "cli": c.cli, "sessions": c.sessions, "turns": c.turns })
            })
            .collect();
        json_result(serde_json::json!({
            "per_cli": per_cli,
            "generation": health.generation,
            "ready": health.ready,
            "files_pending": self.shared.pending_files.load(Ordering::Relaxed),
            "uptime_seconds": health.uptime_seconds,
            "rss_bytes": health.rss_bytes,
            "binary_version": health.version,
            "state_path": health.state_path,
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Search past Claude Code and Codex conversations. Every content tool needs a \
             `cli` argument: claude_code, codex, or all.",
        )
    }
}

pub fn health_of(shared: &Shared) -> Health {
    let generation = shared
        .published
        .lock()
        .map(|published| published.generation)
        .unwrap_or(0);
    Health {
        name: paths::HEALTH_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        state_path: shared.store_dir.to_string_lossy().to_string(),
        generation,
        live: true,
        ready: shared.ready.load(Ordering::Relaxed),
        uptime_seconds: shared.uptime_seconds(),
        rss_bytes: crate::agent::resident_bytes(std::process::id()),
        files_indexed: shared.build_progress.files_done.load(Ordering::Relaxed),
        files_total: shared.build_progress.files_total.load(Ordering::Relaxed),
    }
}

/// Serves MCP on `/mcp` and health on `/healthz`, until SIGINT or SIGTERM.
pub fn serve_blocking(shared: Arc<Shared>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the tokio runtime")?;
    runtime.block_on(serve_until_shutdown(shared))
}

async fn serve_until_shutdown(shared: Arc<Shared>) -> Result<()> {
    let host = format!("127.0.0.1:{}", paths::PORT);
    let localhost = format!("localhost:{}", paths::PORT);
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(vec![host.clone(), localhost.clone()])
        .with_allowed_origins(vec![
            format!("http://{host}"),
            format!("http://{localhost}"),
        ]);

    let tools = Tools::new(Arc::clone(&shared));
    let service: StreamableHttpService<Tools, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(tools.clone()), Default::default(), config);

    let health_shared = Arc::clone(&shared);
    let router = axum::Router::new()
        .route(
            "/healthz",
            axum::routing::get(move || {
                let shared = Arc::clone(&health_shared);
                async move { axum::Json(health_of(&shared)) }
            }),
        )
        .nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(&host)
        .await
        .with_context(|| format!("binding {host}"))?;
    tracing::info!(%host, "http up");

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
            match terminate {
                Ok(mut terminate) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = terminate.recv() => {}
                    }
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
            tracing::info!("shutdown signal received");
        })
        .await
        .context("serving HTTP")
}
