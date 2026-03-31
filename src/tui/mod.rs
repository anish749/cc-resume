mod app;
mod input;
mod ui;

use anyhow::Result;
use tokio::time::Instant;

pub async fn run() -> Result<()> {
    let t0 = Instant::now();
    let config = crate::config::Config::load()?;
    let qmd = crate::qmd::QmdClient::new(&config);

    // Auto-start daemon if not running
    if !crate::watcher::is_running(&config) {
        tracing::info!("Daemon not running, starting...");
        crate::watcher::start_daemon(&config).await?;
    }

    tracing::debug!("TUI startup: {:?}", t0.elapsed());

    let mut terminal = ui::setup_terminal()?;
    let result = app::App::new(qmd).run(&mut terminal).await;
    ui::restore_terminal()?;

    result
}
