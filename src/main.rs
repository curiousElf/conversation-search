//! Command line entry point. The daemon and the terminal commands are the same
//! binary, so nothing outside it needs to know the port or the state directory.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use conversation_search::parse::Cli as ParserCli;
use conversation_search::store::{self, index, meta};
use conversation_search::{agent, daemon, paths, reindex, watch};
use std::sync::atomic::Ordering;

#[derive(Parser)]
#[command(name = "conversation-search", version, about = "Search past Claude Code and Codex conversations")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: watch both transcript roots and serve MCP over HTTP.
    Serve,
    /// Rebuild the index from scratch into a new store. Search is unavailable
    /// while this runs.
    Reindex,
    /// Health, counts, generation, and pending files.
    Status,
    /// Search from the terminal.
    Search {
        query: String,
        /// claude_code, codex, or all.
        #[arg(long, default_value = "all")]
        cli: String,
        #[arg(long, default_value = "")]
        project: String,
        #[arg(long, default_value_t = 10)]
        top_k: usize,
    },
    /// Start the launchd agent, or restart it if it is already loaded.
    Restart,
}

fn main() -> Result<()> {
    let args = Args::parse();
    // Terminal commands log warnings (skipped transcripts, in-place edits) to
    // stderr. The daemon installs its own file sink instead, so it must be the
    // only one to set the global subscriber.
    if !matches!(args.command, Command::Serve) {
        start_terminal_logging();
    }
    match args.command {
        Command::Serve => daemon::run(),
        Command::Reindex => run_reindex(),
        Command::Status => print_status(),
        Command::Search { query, cli, project, top_k } => {
            run_search(&query, &cli, &project, top_k)
        }
        Command::Restart => restart_agent(),
    }
}

fn start_terminal_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tantivy=warn")),
        )
        .init();
}

/// `all` means no CLI filter; anything else must name one of the two.
fn cli_filter(name: &str) -> Result<Option<ParserCli>> {
    match name {
        "all" => Ok(None),
        other => match ParserCli::from_str(other) {
            Some(cli) => Ok(Some(cli)),
            None => bail!("cli must be claude_code, codex, or all (got {other:?})"),
        },
    }
}

fn run_reindex() -> Result<()> {
    let was_loaded = agent::agent_is_loaded();
    if was_loaded {
        println!("Stopping the daemon");
        agent::bootout_agent();
    }
    // Whatever happens below, the agent goes back up and `current` stays where
    // it was unless the new store is complete.
    let result = build_and_switch();
    if was_loaded {
        println!("Starting the daemon");
        agent::bootstrap_agent();
    }
    result
}

fn build_and_switch() -> Result<()> {
    let store_dir = store::create_new_store_dir()?;
    println!("Building {}", store_dir.display());

    let roots = watch::roots_from_home()?;
    let progress = reindex::Progress::default();
    let summary = std::thread::scope(|scope| {
        let handle = scope.spawn(|| reindex::build_store(&store_dir, &roots, &progress));
        while !handle.is_finished() {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let done = progress.files_done.load(Ordering::Relaxed);
            let total = progress.files_total.load(Ordering::Relaxed);
            let turns = progress.turns_indexed.load(Ordering::Relaxed);
            if total > 0 {
                println!("  {done}/{total} files, {turns} turns");
            }
        }
        handle.join().expect("the build thread panicked")
    })?;

    if let Some(previous) = store::current_store_dir()? {
        std::fs::write(paths::previous_store_marker()?, previous.to_string_lossy().as_bytes())
            .context("recording the previous store")?;
    }
    store::switch_current(&store_dir)?;
    println!(
        "Indexed {} turns from {} files in {:.1}s",
        summary.turns, summary.files, summary.seconds
    );
    for removed in store::prune_stores()? {
        println!("Removed old store {}", removed.display());
    }
    Ok(())
}

fn print_status() -> Result<()> {
    match agent::fetch_health() {
        Some(health) => {
            let state = if health.ready {
                "ready".to_string()
            } else {
                format!("live, building ({}/{})", health.files_indexed, health.files_total)
            };
            println!("daemon:     {state} (pid {})", health.pid);
            println!("version:    {}", health.version);
            println!("generation: {}", health.generation);
            println!("uptime:     {}s", health.uptime_seconds);
            println!("rss:        {} MB", health.rss_bytes / 1_048_576);
        }
        None => {
            println!("daemon:     not answering on {}", paths::health_url());
            if let Some(reason) = agent::last_permanent_failure() {
                println!("last stop:  {reason}");
            }
        }
    }

    match store::open_current() {
        Ok(store) => {
            println!("store:      {}", store.dir.display());
            println!("generation: {}", meta::generation(&store.db)?);
            for counts in meta::counts_by_cli(&store.db)? {
                println!(
                    "  {:<12} {:>6} sessions  {:>8} turns",
                    counts.cli, counts.sessions, counts.turns
                );
            }
        }
        Err(error) => println!("store:      unavailable ({error:#})"),
    }
    Ok(())
}

fn run_search(query: &str, cli: &str, project: &str, top_k: usize) -> Result<()> {
    let filter = cli_filter(cli)?;
    let store = store::open_current()?;
    let reader = index::reader(&store.index)?;
    let searcher = reader.searcher();
    let hits = index::search(&searcher, &store.fields, query, filter, project, top_k)?;

    if hits.is_empty() {
        println!("no matches");
        return Ok(());
    }
    for hit in hits {
        println!(
            "[{:.3}] {} {} turn {} {}",
            hit.score, hit.cli, hit.project, hit.turn_number, hit.timestamp
        );
        println!("        {}", snippet(&hit.text, 200));
        println!("        turn_id={}", hit.turn_id);
    }
    Ok(())
}

fn snippet(text: &str, limit: usize) -> String {
    let flattened = text.replace('\n', " ");
    if flattened.chars().count() <= limit {
        flattened
    } else {
        flattened.chars().take(limit).collect::<String>() + "…"
    }
}

fn restart_agent() -> Result<()> {
    if agent::agent_is_loaded() {
        agent::kickstart_agent();
        println!("Restarted {}", paths::LAUNCHD_LABEL);
    } else {
        agent::bootstrap_agent();
        println!("Started {}", paths::LAUNCHD_LABEL);
    }
    Ok(())
}
