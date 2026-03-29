mod cli;
mod config;
mod exporter;
mod qmd;
mod search;
mod session;
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

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("claude_resume=info".parse()?),
        )
        .init();

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
        None => {
            tui::run().await?;
        }
    }

    Ok(())
}
