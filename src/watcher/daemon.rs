use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebouncedEvent};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::summarizer::{self, SummarizeQueue};

/// Start the daemon in the background by re-exec'ing ourselves with `_watch`.
/// This is a no-op if the daemon is already running.
pub async fn start(config: &Config) -> Result<()> {
    if is_running(config) {
        println!("Daemon is already running (PID: {})", read_pid(config).unwrap_or(0));
        return Ok(());
    }

    let data_dir = config.daemon_pid_file().parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&data_dir)?;

    let log_file = std::fs::File::create(config.daemon_log_file())
        .context("Failed to create daemon log file")?;
    let log_file_err = log_file.try_clone()?;

    let child = std::process::Command::new(std::env::current_exe()?)
        .arg("_watch")
        .stdout(log_file)
        .stderr(log_file_err)
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn daemon process")?;

    let pid = child.id();
    std::fs::write(config.daemon_pid_file(), pid.to_string())?;
    println!("Daemon started (PID: {pid})");
    println!("Log: {}", config.daemon_log_file().display());

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

/// Print daemon status.
pub fn status(config: &Config) -> Result<()> {
    match read_pid(config) {
        Some(pid) if process_alive(pid) => {
            println!("Daemon is running (PID: {pid})");
            println!("Log: {}", config.daemon_log_file().display());
        }
        Some(pid) => {
            println!("Daemon is not running (stale PID {pid}, cleaning up).");
            cleanup_pid_file(config);
        }
        None => {
            println!("Daemon is not running.");
        }
    }
    Ok(())
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
/// 2. Watches for JSONL file changes in the Claude projects directory
/// 3. Re-exports changed sessions to markdown
/// 4. Triggers QMD reindex after a batch of changes settles
/// 5. Every 15 minutes, scans for sessions needing summarization
/// 6. Processes one summarization job at a time in a background task
pub async fn run_watcher(config: &Config) -> Result<()> {
    tracing::info!("Daemon starting");

    // Step 1: Ensure QMD daemon is running
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

    // Step 2: Set up file watcher
    let projects_dir = config.claude_projects_dir();
    if !projects_dir.is_dir() {
        tracing::info!("Projects dir doesn't exist yet: {}. Will watch for it.", projects_dir.display());
        // Wait for it to appear
        loop {
            if projects_dir.is_dir() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    let export_dir = config.export_dir();
    std::fs::create_dir_all(&export_dir)?;

    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel::<Vec<DebouncedEvent>>();

    let mut debouncer = new_debouncer(
        Duration::from_secs(3),
        None,
        move |events: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
            if let Ok(events) = events {
                let _ = fs_tx.send(events);
            }
        },
    )?;

    debouncer
        .watch(&projects_dir, RecursiveMode::Recursive)
        .with_context(|| format!("Failed to watch {}", projects_dir.display()))?;

    tracing::info!("Watching {} for changes", projects_dir.display());

    // Step 3: Set up summarization queue
    let summarize_queue = Arc::new(SummarizeQueue::new());
    let mut summarizer_busy = false;
    let mut summarizer_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut last_summary_scan = tokio::time::Instant::now();
    let summary_scan_interval = Duration::from_secs(15 * 60); // 15 minutes

    // Initial enqueue: populate queue with all unsummarized sessions (newest first)
    tracing::info!("Scanning sessions for initial summarization backlog...");
    match summarizer::enqueue_pending(config, &summarize_queue).await {
        Ok(n) => {
            if n > 0 {
                tracing::info!("Enqueued {n} sessions for summarization");
            }
        }
        Err(e) => tracing::warn!("Failed initial summary scan: {e}"),
    }

    // Step 4: Event loop — process file changes, batch QMD reindexes, summarization
    let mut pending_reindex = false;
    let mut last_export = tokio::time::Instant::now();

    loop {
        // Check if background summarizer task completed
        if let Some(ref handle) = summarizer_handle {
            if handle.is_finished() {
                summarizer_handle = None;
                summarizer_busy = false;
            }
        }

        tokio::select! {
            Some(events) = fs_rx.recv() => {
                for event in events {
                    for path in &event.paths {
                        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                            continue;
                        }
                        if let Some(result) = extract_session_info(path, &projects_dir) {
                            let output_path = export_dir.join(format!("{}.md", result.session_id));
                            match crate::exporter::export_session(
                                path,
                                &output_path,
                                &result.project_name,
                                &result.session_id,
                            ) {
                                Ok(_) => {
                                    tracing::debug!("Exported {}", result.session_id);
                                    pending_reindex = true;
                                    last_export = tokio::time::Instant::now();
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to export {}: {e}", path.display());
                                }
                            }
                        }
                    }
                }
            }

            // Tick every second for housekeeping
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                // Batch reindex: wait 10s after last export before triggering QMD
                if pending_reindex
                    && tokio::time::Instant::now().duration_since(last_export) >= Duration::from_secs(10)
                {
                    pending_reindex = false;
                    tracing::info!("Triggering QMD reindex");
                    if let Err(e) = reindex_qmd().await {
                        tracing::warn!("QMD reindex failed: {e}");
                    }
                }

                // Every 15 minutes: scan for sessions needing summarization
                if tokio::time::Instant::now().duration_since(last_summary_scan) >= summary_scan_interval {
                    last_summary_scan = tokio::time::Instant::now();
                    tracing::info!("Periodic summary scan...");
                    match summarizer::enqueue_pending(config, &summarize_queue).await {
                        Ok(n) => {
                            if n > 0 {
                                tracing::info!("Enqueued {n} sessions for summarization");
                            }
                        }
                        Err(e) => tracing::warn!("Summary scan failed: {e}"),
                    }
                }

                // Pop one job from the queue if summarizer is idle
                if !summarizer_busy {
                    if let Some(job) = summarize_queue.pop().await {
                        summarizer_busy = true;
                        let config_clone = config.clone();
                        let queue_clone = Arc::clone(&summarize_queue);
                        summarizer_handle = Some(tokio::spawn(async move {
                            tracing::info!(
                                "Summarizing session {} (update={})",
                                job.session_id,
                                job.is_update
                            );
                            match summarizer::summarize_session(&config_clone, &job).await {
                                Ok(summary) => {
                                    if let Err(e) = summarizer::write_summary(&config_clone, &summary) {
                                        tracing::warn!(
                                            "Failed to write summary for {}: {e}",
                                            job.session_id
                                        );
                                    } else {
                                        let remaining = queue_clone.len().await;
                                        tracing::info!(
                                            "Summarized {} ({} remaining in queue)",
                                            job.session_id,
                                            remaining
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to summarize {}: {e}",
                                        job.session_id
                                    );
                                    // Job is dropped; the next periodic scan will
                                    // re-discover it if it still needs summarization.
                                }
                            }
                        }));
                    }
                }
            }
        }
    }
}

struct SessionInfo {
    project_name: String,
    session_id: String,
}

/// Extract project name and session ID from a JSONL file path.
/// Expected: <projects_dir>/<project_name>/<session_id>.jsonl
fn extract_session_info(path: &Path, projects_dir: &Path) -> Option<SessionInfo> {
    let relative = path.strip_prefix(projects_dir).ok()?;
    let components: Vec<_> = relative.components().collect();
    if components.len() != 2 {
        return None;
    }
    let project_name = components[0].as_os_str().to_string_lossy().to_string();
    let session_id = path
        .file_stem()?
        .to_string_lossy()
        .to_string();
    Some(SessionInfo {
        project_name,
        session_id,
    })
}

/// Run `qmd update` followed by `qmd embed`.
async fn reindex_qmd() -> Result<()> {
    let update_status = tokio::process::Command::new("qmd")
        .arg("update")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;

    if !update_status.success() {
        anyhow::bail!("qmd update failed with status {update_status}");
    }

    let embed_status = tokio::process::Command::new("qmd")
        .arg("embed")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;

    if !embed_status.success() {
        anyhow::bail!("qmd embed failed with status {embed_status}");
    }

    tracing::info!("QMD reindex complete");
    Ok(())
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
