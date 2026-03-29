mod daemon;

use anyhow::Result;

use crate::config::Config;

pub use daemon::run_watcher;

pub async fn start_daemon(config: &Config) -> Result<()> {
    daemon::start(config).await
}

pub fn stop_daemon(config: &Config) -> Result<()> {
    daemon::stop(config)
}

pub fn daemon_status(config: &Config) -> Result<()> {
    daemon::status(config)
}
