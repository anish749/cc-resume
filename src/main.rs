mod cli;
mod config;
mod exporter;
mod pipeline;
mod qmd;
mod session;
mod summarizer;
mod tui;
mod watcher;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    // Internal daemon command: runs the file watcher in foreground.
    // This is invoked by `daemon start` which re-execs with `_watch`.
    if std::env::args().nth(1).as_deref() == Some("_watch") {
        tracing_subscriber::fmt()
            .with_env_filter("claude_resume=info")
            .init();
        let config = config::Config::load()?;
        return watcher::run_watcher(&config).await;
    }

    // Log to file if CLAUDE_RESUME_LOG is set, otherwise stderr.
    // For TUI mode, stderr is swallowed by the alternate screen,
    // so use: CLAUDE_RESUME_LOG=/tmp/cr.log claude-resume
    use tracing_subscriber::fmt::format::FmtSpan;

    if let Ok(log_path) = std::env::var("CLAUDE_RESUME_LOG") {
        let file = std::fs::File::create(&log_path)?;
        tracing_subscriber::fmt()
            .with_env_filter("claude_resume=debug")
            .with_writer(file)
            .with_ansi(false)
            .with_span_events(FmtSpan::CLOSE)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("claude_resume=info".parse()?),
            )
            .with_span_events(FmtSpan::CLOSE)
            .init();
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Search { query, limit }) => {
            cli::handle_search(&query, limit).await?;
        }
        Some(Commands::Index { full }) => {
            cli::handle_index(full).await?;
        }
        Some(Commands::Daemon { action }) => {
            cli::handle_daemon(action).await?;
        }
        Some(Commands::Setup) => {
            cli::handle_setup().await?;
        }
        Some(Commands::Summarize { full }) => {
            cli::handle_summarize(full).await?;
        }
        None => {
            tui::run().await?;
        }
    }

    Ok(())
}
