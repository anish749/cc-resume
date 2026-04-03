use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;

/// Start the daemon in the background by re-exec'ing ourselves with `_watch`.
/// This is a no-op if the daemon is already running.
pub async fn start(config: &Config) -> Result<()> {
    if is_running(config) {
        println!("Daemon is already running (PID: {})", read_pid(config).unwrap_or(0));
        return Ok(());
    }

    let data_dir = config.daemon_pid_file().parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(config.daemon_log_dir())?;

    let child = std::process::Command::new(std::env::current_exe()?)
        .arg("_watch")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn daemon process")?;

    let pid = child.id();
    std::fs::write(config.daemon_pid_file(), pid.to_string())?;
    println!("Daemon started (PID: {pid})");
    println!("Logs: {}", config.daemon_log_dir().display());

    Ok(())
}

/// Stop the daemon. Leaves QMD's daemon running (user may use it for other things).
pub fn stop(config: &Config) -> Result<()> {
    let pid = match read_pid(config) {
        Some(pid) => pid,
        None => {
            println!("Daemon is not running.");
            return Ok(());
        }
    };

    if !process_alive(pid) {
        cleanup_pid_file(config);
        println!("Daemon was not running (stale PID file cleaned up).");
        return Ok(());
    }

    // Send SIGTERM
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }

    // Wait up to 3 seconds for it to exit
    for _ in 0..30 {
        if !process_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    cleanup_pid_file(config);
    println!("Daemon stopped (PID: {pid}).");
    Ok(())
}

/// Print daemon status with summarization stats.
pub fn status(config: &Config) -> Result<()> {
    match read_pid(config) {
        Some(pid) if process_alive(pid) => {
            println!("Daemon is running (PID: {pid})");
            println!("Logs: {}", config.daemon_log_dir().display());
            print_summary_stats(config);
        }
        Some(pid) => {
            println!("Daemon is not running (stale PID {pid}, cleaning up).");
            cleanup_pid_file(config);
        }
        None => {
            println!("Daemon is not running.");
            print_summary_stats(config);
        }
    }
    Ok(())
}

/// Print session and summarization statistics.
fn print_summary_stats(config: &Config) {
    let sessions_dir = config.export_dir();
    let summaries_dir = config.summaries_dir();

    let session_count = std::fs::read_dir(&sessions_dir)
        .map(|entries| entries.filter_map(|e| e.ok()).filter(|e| {
            e.path().extension().and_then(|ext| ext.to_str()) == Some("md")
        }).count())
        .unwrap_or(0);

    let summary_count = std::fs::read_dir(&summaries_dir)
        .map(|entries| entries.filter_map(|e| e.ok()).filter(|e| {
            e.path().extension().and_then(|ext| ext.to_str()) == Some("yaml")
        }).count())
        .unwrap_or(0);

    let below_threshold = session_count.saturating_sub(summary_count);

    println!("\nSessions: {session_count} exported, {summary_count} summarized, {below_threshold} below threshold");
}

/// Check if the daemon is running.
pub fn is_running(config: &Config) -> bool {
    match read_pid(config) {
        Some(pid) if process_alive(pid) => true,
        Some(_) => {
            // Stale PID file
            cleanup_pid_file(config);
            false
        }
        None => false,
    }
}

/// The main watcher loop. This runs in the foreground of the daemon process.
///
/// 1. Ensures the QMD HTTP daemon is running (for fast searches)
/// 2. Runs the indexing pipeline every 10 minutes:
///    scan stale (mtime) → export → summarize → inject → reindex
pub async fn run_watcher(config: &Config) -> Result<()> {
    tracing::info!("Daemon starting");

    let qmd = crate::qmd::QmdClient::new(config);
    if qmd.is_installed() {
        tracing::info!("Starting QMD daemon for fast searches...");
        if let Err(e) = qmd.start_daemon().await {
            tracing::warn!("Failed to start QMD daemon: {e}. Searches will be slow.");
        } else {
            tracing::info!("QMD daemon is running");
        }
    } else {
        tracing::warn!("QMD is not installed. Searches will not work.");
    }

    let interval = Duration::from_secs(10 * 60); // 10 minutes

    // Run pipeline immediately on startup, then every 10 minutes.
    loop {
        match crate::pipeline::run(config).await {
            Ok(n) if n > 0 => tracing::info!("Pipeline complete: {n} sessions processed"),
            Ok(_) => tracing::debug!("Pipeline complete: nothing to do"),
            Err(e) => tracing::warn!("Pipeline failed: {e}"),
        }

        tokio::time::sleep(interval).await;
    }
}

fn read_pid(config: &Config) -> Option<u32> {
    std::fs::read_to_string(config.daemon_pid_file())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn cleanup_pid_file(config: &Config) {
    let _ = std::fs::remove_file(config.daemon_pid_file());
}
