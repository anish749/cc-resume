mod app;
mod input;
mod ui;

use anyhow::Result;
use tokio::time::Instant;

pub async fn run() -> Result<()> {
    let config = crate::config::Config::load()?;
    let qmd = crate::qmd::QmdClient::new(&config);

    if !qmd.is_installed() {
        anyhow::bail!(
            "QMD is not installed. Run `claude-resume setup` for guided installation."
        );
    }

    // Auto-start daemon if not running
    if !crate::watcher::is_running(&config) {
        eprintln!("Starting daemon (file watcher + QMD model cache)...");
        crate::watcher::start_daemon(&config).await?;
    }

    // Warm up QMD models by running a real query.
    // Cold start loads ~2GB of models into VRAM; this blocks until they're ready.
    eprintln!("Warming up search models (first time may take 20-30s)...");
    let start = Instant::now();
    let warmup = tokio::process::Command::new("qmd")
        .args(["query", "test", "-c", "claude-sessions", "-n", "1", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    let elapsed = start.elapsed();
    match warmup {
        Ok(status) if status.success() => {
            eprintln!("Models ready ({:.1}s).", elapsed.as_secs_f64());
        }
        Ok(_) => {
            eprintln!("Warning: warmup query failed. Search may not work correctly.");
        }
        Err(e) => {
            eprintln!("Warning: could not run warmup query: {e}");
        }
    }

    let mut terminal = ui::setup_terminal()?;
    let result = app::App::new(qmd).run(&mut terminal).await;
    ui::restore_terminal()?;

    result
}
