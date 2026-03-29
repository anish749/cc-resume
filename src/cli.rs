use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "claude-resume", about = "Semantic search and resume for Claude Code sessions")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Search sessions by query
    Search {
        /// The search query
        query: String,
        /// Max number of results
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
    },
    /// Index sessions (full reindex or incremental)
    Index {
        /// Force full reindex
        #[arg(long)]
        full: bool,
    },
    /// Manage the file watcher daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Guided setup: check QMD installation, create collection, initial index
    Setup,
}

#[derive(Subcommand, Clone)]
pub enum DaemonAction {
    /// Start the file watcher daemon
    Start,
    /// Stop the file watcher daemon
    Stop,
    /// Show daemon status
    Status,
}

pub async fn handle_search(query: &str, limit: usize) -> Result<()> {
    let config = crate::config::Config::load()?;
    let qmd = crate::qmd::QmdClient::new(&config);

    if !qmd.is_installed() {
        anyhow::bail!(
            "QMD is not installed. Run `claude-resume setup` for guided installation."
        );
    }

    let results = qmd.deep_search(query, limit).await?;

    if results.is_empty() {
        println!("No sessions found for: {query}");
        return Ok(());
    }

    for result in &results {
        println!(
            "{score:.0}%  {date}  {project}",
            score = result.score * 100.0,
            date = result.date.as_deref().unwrap_or("unknown"),
            project = result.project_path.as_deref().unwrap_or("unknown"),
        );
        if let Some(ref prompt) = result.first_prompt {
            let truncated: String = prompt.chars().take(80).collect();
            println!("     {truncated}");
        }
        println!();
    }

    Ok(())
}

pub async fn handle_index(full: bool) -> Result<()> {
    let config = crate::config::Config::load()?;
    let qmd = crate::qmd::QmdClient::new(&config);

    if !qmd.is_installed() {
        anyhow::bail!(
            "QMD is not installed. Run `claude-resume setup` for guided installation."
        );
    }

    println!("Indexing sessions...");
    let stats = crate::exporter::export_all(&config, full).await?;
    println!(
        "Exported {} new sessions ({} skipped)",
        stats.exported, stats.skipped
    );

    println!("Updating QMD index...");
    qmd.update().await?;

    println!("Generating embeddings...");
    qmd.embed().await?;

    println!("Done.");
    Ok(())
}

pub async fn handle_daemon(action: DaemonAction) -> Result<()> {
    let config = crate::config::Config::load()?;

    match action {
        DaemonAction::Start => crate::watcher::start_daemon(&config).await?,
        DaemonAction::Stop => crate::watcher::stop_daemon(&config)?,
        DaemonAction::Status => crate::watcher::daemon_status(&config)?,
    }

    Ok(())
}

pub async fn handle_setup() -> Result<()> {
    let config = crate::config::Config::load()?;

    println!("=== claude-resume setup ===\n");

    // Step 1: Check QMD
    let qmd = crate::qmd::QmdClient::new(&config);
    if qmd.is_installed() {
        println!("[ok] QMD is installed");
    } else {
        println!("[!!] QMD is not installed.");
        println!("     Install it with: npm install -g @tobilu/qmd");
        println!("     Then re-run: claude-resume setup");
        return Ok(());
    }

    // Step 2: Create collection
    println!("\nCreating QMD collection...");
    qmd.ensure_collection().await?;
    println!("[ok] Collection '{}' ready", config.qmd_collection_name());

    // Step 3: Initial index
    println!("\nThis will index all your existing Claude Code sessions.");
    println!("Sessions are stored in: {}", config.claude_projects_dir().display());
    println!("Exported markdown will go to: {}", config.export_dir().display());
    println!("\nIndexing...");

    let stats = crate::exporter::export_all(&config, true).await?;
    println!(
        "Exported {} sessions ({} skipped)",
        stats.exported, stats.skipped
    );

    println!("\nUpdating QMD index and generating embeddings...");
    println!("(This may take a while on first run as QMD downloads its ~330MB embedding model)");
    qmd.update().await?;
    qmd.embed().await?;

    println!("\n[ok] Setup complete!");
    println!("\nUsage:");
    println!("  claude-resume              Launch TUI search");
    println!("  claude-resume search \"q\"   CLI search");
    println!("  claude-resume daemon start  Start file watcher for live indexing");

    Ok(())
}
