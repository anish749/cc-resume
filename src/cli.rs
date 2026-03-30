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
    /// Summarize sessions (generate AI summaries for sessions that need them)
    Summarize {
        /// Force re-summarize all sessions (ignore existing summaries)
        #[arg(long)]
        full: bool,
    },
}

#[derive(Subcommand, Clone)]
pub enum DaemonAction {
    /// Start the file watcher daemon
    Start,
    /// Stop the file watcher daemon
    Stop,
    /// Restart the file watcher daemon
    Restart,
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

    let results = qmd.search(query, limit).await?;

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
        DaemonAction::Restart => {
            crate::watcher::stop_daemon(&config)?;
            crate::watcher::start_daemon(&config).await?;
        }
        DaemonAction::Status => crate::watcher::daemon_status(&config)?,
    }

    Ok(())
}

pub async fn handle_summarize(full: bool) -> Result<()> {
    let config = crate::config::Config::load()?;

    // Check claude CLI is available
    let claude_check = tokio::process::Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    if !claude_check.map_or(false, |s| s.success()) {
        anyhow::bail!("Claude CLI is not installed or not on PATH. It's required for summarization.");
    }

    let sessions_dir = config.export_dir();
    if !sessions_dir.is_dir() {
        anyhow::bail!(
            "Sessions directory not found: {}. Run `claude-resume index` first.",
            sessions_dir.display()
        );
    }

    if full {
        // Delete all existing summaries to force re-generation
        let summaries_dir = config.summaries_dir();
        if summaries_dir.is_dir() {
            println!("Clearing existing summaries...");
            std::fs::remove_dir_all(&summaries_dir)?;
        }
    }

    let queue = crate::summarizer::SummarizeQueue::new();
    let enqueued = crate::summarizer::enqueue_pending(&config, &queue).await?;

    if enqueued == 0 {
        println!("All sessions are up to date. Nothing to summarize.");
        return Ok(());
    }

    println!("Summarizing {enqueued} sessions...\n");

    let mut done = 0;
    let mut errors = 0;
    while let Some(job) = queue.pop().await {
        print!(
            "[{}/{}] {} ({})",
            done + 1,
            enqueued,
            &job.session_id[..8.min(job.session_id.len())],
            if job.is_update { "update" } else { "initial" }
        );

        match crate::summarizer::summarize_session(&config, &job).await {
            Ok(summary) => {
                crate::summarizer::write_summary(&config, &summary)?;
                println!(" ✓");
                done += 1;
            }
            Err(e) => {
                println!(" ✗ {e}");
                errors += 1;
            }
        }
    }

    println!("\nDone: {done} summarized, {errors} errors.");
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

    // Step 4: Start daemon (file watcher + QMD model cache)
    println!("\nStarting daemon (file watcher + QMD model cache)...");
    crate::watcher::start_daemon(&config).await?;
    println!("[ok] Daemon running — new sessions will be indexed automatically");

    println!("\n[ok] Setup complete!");
    println!("\nUsage:");
    println!("  claude-resume              Launch TUI search");
    println!("  claude-resume search \"q\"   CLI search");
    println!("  claude-resume daemon stop   Stop the background daemon");

    Ok(())
}
