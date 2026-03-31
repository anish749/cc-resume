mod app;
mod folder_tree;
mod input;
mod ui;

use anyhow::Result;

pub async fn run() -> Result<()> {
    let config = crate::config::Config::load()?;
    let qmd = crate::qmd::QmdClient::new(&config);

    // Auto-start daemon if not running
    if !crate::watcher::is_running(&config) {
        tracing::info!("Daemon not running, starting...");
        crate::watcher::start_daemon(&config).await?;
    }

    let mut terminal = ui::setup_terminal()?;
    let result = app::App::new(qmd).run(&mut terminal).await;
    ui::restore_terminal()?;

    result
}
