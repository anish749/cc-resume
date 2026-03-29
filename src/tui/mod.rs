mod app;
mod input;
mod ui;

use anyhow::Result;

pub async fn run() -> Result<()> {
    let config = crate::config::Config::load()?;
    let qmd = crate::qmd::QmdClient::new(&config);

    if !qmd.is_installed() {
        anyhow::bail!(
            "QMD is not installed. Run `claude-resume setup` for guided installation."
        );
    }

    let mut terminal = ui::setup_terminal()?;
    let result = app::App::new(config, qmd).run(&mut terminal).await;
    ui::restore_terminal()?;

    result
}
