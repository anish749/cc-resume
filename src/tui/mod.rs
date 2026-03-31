mod app;
mod input;
mod ui;

use anyhow::Result;
use tokio::time::Instant;

pub async fn run() -> Result<()> {
    let t0 = Instant::now();
    let config = crate::config::Config::load()?;
    let qmd = crate::qmd::QmdClient::new(&config);
    tracing::info!("Config loaded: {:?}", t0.elapsed());

    let t1 = Instant::now();
    if !qmd.is_installed() {
        anyhow::bail!(
            "QMD is not installed. Run `claude-resume setup` for guided installation."
        );
    }
    tracing::info!("is_installed check: {:?}", t1.elapsed());

    // Auto-start daemon if not running
    let t2 = Instant::now();
    if !crate::watcher::is_running(&config) {
        eprintln!("Starting daemon...");
        tracing::info!("Daemon not running, starting...");
        crate::watcher::start_daemon(&config).await?;
    }
    tracing::info!("Daemon check: {:?}", t2.elapsed());

    // Warm up QMD models by running a real search through the MCP daemon.
    eprintln!("Warming up search models (first time may take 20-30s)...");
    let t3 = Instant::now();
    match qmd.search("test", 1).await {
        Ok(_) => {
            tracing::info!("Warmup search: {:?}", t3.elapsed());
            eprintln!("Models ready ({:.1}s).", t3.elapsed().as_secs_f64());
        }
        Err(e) => {
            tracing::warn!("Warmup search failed after {:?}: {e}", t3.elapsed());
            eprintln!("Warning: warmup search failed: {e}");
        }
    }

    tracing::info!("Total TUI startup: {:?}", t0.elapsed());

    let mut terminal = ui::setup_terminal()?;
    let result = app::App::new(qmd).run(&mut terminal).await;
    ui::restore_terminal()?;

    result
}
