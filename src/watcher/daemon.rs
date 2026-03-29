use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};

use crate::config::Config;

/// Check whether a process with the given PID is alive.
fn is_process_alive(pid: u32) -> bool {
    // kill with signal 0 checks existence without sending a real signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Read the PID from the pid file, returning None if the file doesn't exist or is malformed.
fn read_pid(pid_path: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Start the watcher daemon as a background process.
///
/// This re-invokes the current executable with the internal `_watch` subcommand,
/// detaching stdout/stderr to the log file.
pub async fn start(config: &Config) -> Result<()> {
    let pid_path = config.daemon_pid_file();

    // Check if already running.
    if let Some(pid) = read_pid(&pid_path) {
        if is_process_alive(pid) {
            println!("Daemon is already running (PID: {pid})");
            return Ok(());
        }
        // Stale PID file — remove it.
        tracing::info!("Removing stale PID file (PID {pid} is not running)");
        let _ = std::fs::remove_file(&pid_path);
    }

    // Ensure directories exist.
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)
            .context("Failed to create directory for PID file")?;
    }

    let log_file_path = config.daemon_log_file();
    if let Some(parent) = log_file_path.parent() {
        std::fs::create_dir_all(parent)
            .context("Failed to create directory for log file")?;
    }

    let exe = std::env::current_exe().context("Failed to determine current executable")?;

    let stdout_file = std::fs::File::create(&log_file_path)
        .with_context(|| format!("Failed to create log file: {}", log_file_path.display()))?;
    let stderr_file = stdout_file
        .try_clone()
        .context("Failed to clone log file handle")?;

    let child = std::process::Command::new(exe)
        .arg("_watch")
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
        .context("Failed to spawn daemon process")?;

    let pid = child.id();
    std::fs::write(&pid_path, pid.to_string())
        .with_context(|| format!("Failed to write PID file: {}", pid_path.display()))?;

    println!("Daemon started (PID: {pid})");
    println!("Logs: {}", log_file_path.display());

    Ok(())
}

/// Stop a running daemon by sending SIGTERM.
pub fn stop(config: &Config) -> Result<()> {
    let pid_path = config.daemon_pid_file();

    let pid = match read_pid(&pid_path) {
        Some(pid) => pid,
        None => {
            println!("Daemon is not running (no PID file)");
            return Ok(());
        }
    };

    if !is_process_alive(pid) {
        // Process is gone, clean up the stale PID file.
        let _ = std::fs::remove_file(&pid_path);
        println!("Daemon is not running (stale PID file removed)");
        return Ok(());
    }

    // Send SIGTERM.
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        anyhow::bail!("Failed to send SIGTERM to PID {pid}: {err}");
    }

    // Wait briefly for the process to exit.
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        if !is_process_alive(pid) {
            break;
        }
    }

    let _ = std::fs::remove_file(&pid_path);
    println!("Daemon stopped (PID: {pid})");

    Ok(())
}

/// Print the current daemon status.
pub fn status(config: &Config) -> Result<()> {
    let pid_path = config.daemon_pid_file();

    match read_pid(&pid_path) {
        Some(pid) => {
            if is_process_alive(pid) {
                println!("Daemon is running (PID: {pid})");
                println!("PID file: {}", pid_path.display());
                println!("Log file: {}", config.daemon_log_file().display());
            } else {
                println!("Daemon is not running (stale PID file for PID {pid})");
                // Clean up.
                let _ = std::fs::remove_file(&pid_path);
            }
        }
        None => {
            println!("Daemon is not running");
        }
    }

    Ok(())
}

/// A pending export job: a JSONL file that changed and needs re-exporting.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ExportJob {
    jsonl_path: PathBuf,
    project_name: String,
    session_id: String,
}

/// The main watcher loop. This is called inside the forked daemon process.
///
/// Watches `config.claude_projects_dir()` recursively for `.jsonl` file changes,
/// debounces them, re-exports each changed session to markdown, and then triggers
/// a batched QMD reindex after a quiet period.
pub async fn run_watcher(config: &Config) -> Result<()> {
    let projects_dir = config.claude_projects_dir();
    let export_dir = config.export_dir().clone();
    let qmd_collection = config.qmd_collection_name().to_string();

    tracing::info!(
        "Watcher starting — watching {}",
        projects_dir.display()
    );

    // Create the projects directory if it doesn't exist yet. Claude Code will create
    // it eventually, but the watcher should be resilient to starting before that.
    if !projects_dir.is_dir() {
        tracing::warn!(
            "Projects directory does not exist yet: {}. Creating it.",
            projects_dir.display()
        );
        std::fs::create_dir_all(&projects_dir)
            .with_context(|| {
                format!(
                    "Failed to create projects directory: {}",
                    projects_dir.display()
                )
            })?;
    }

    // Ensure export dir exists.
    std::fs::create_dir_all(&export_dir)
        .with_context(|| format!("Failed to create export directory: {}", export_dir.display()))?;

    // Channel for receiving debounced file events.
    let (tx, rx) = std::sync::mpsc::channel();

    // Create a debounced watcher with a 3-second debounce window.
    let mut debouncer = new_debouncer(
        Duration::from_secs(3),
        None,
        move |result: DebounceEventResult| {
            if let Err(e) = tx.send(result) {
                tracing::error!("Failed to send debounced event: {e}");
            }
        },
    )
    .context("Failed to create file watcher")?;

    debouncer
        .watch(&projects_dir, RecursiveMode::Recursive)
        .with_context(|| {
            format!(
                "Failed to watch directory: {}",
                projects_dir.display()
            )
        })?;

    tracing::info!("File watcher active");

    // QMD batching state: we collect exports and trigger QMD reindex after a quiet period.
    let qmd_batch_delay = Duration::from_secs(10);
    let mut pending_qmd_reindex = false;
    let mut last_export_time: Option<Instant> = None;

    loop {
        // Use a short timeout so we can periodically check whether to trigger QMD.
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(events)) => {
                let mut exported_any = false;

                for event in &events {
                    // We only care about paths that are .jsonl files.
                    let paths: Vec<&PathBuf> = event
                        .event
                        .paths
                        .iter()
                        .filter(|p| {
                            p.extension()
                                .and_then(|e| e.to_str())
                                == Some("jsonl")
                        })
                        .collect();

                    for jsonl_path in paths {
                        if let Some(job) = extract_export_job(jsonl_path, &projects_dir) {
                            tracing::info!(
                                "Change detected: project={}, session={}",
                                job.project_name,
                                job.session_id
                            );

                            let output_path =
                                export_dir.join(format!("{}.md", job.session_id));

                            match crate::exporter::export_session(
                                &job.jsonl_path,
                                &output_path,
                                &job.project_name,
                                &job.session_id,
                            ) {
                                Ok(()) => {
                                    tracing::info!(
                                        "Exported session {} -> {}",
                                        job.session_id,
                                        output_path.display()
                                    );
                                    exported_any = true;
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to export session {}: {e:#}",
                                        job.session_id
                                    );
                                }
                            }
                        }
                    }
                }

                if exported_any {
                    last_export_time = Some(Instant::now());
                    pending_qmd_reindex = true;
                }
            }
            Ok(Err(errors)) => {
                for e in &errors {
                    tracing::error!("File watch error: {e}");
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Normal timeout — fall through to QMD batch check.
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                tracing::error!("File watcher channel disconnected, exiting");
                break;
            }
        }

        // Check if we should trigger a batched QMD reindex.
        if pending_qmd_reindex {
            if let Some(last) = last_export_time {
                if last.elapsed() >= qmd_batch_delay {
                    tracing::info!("Triggering QMD reindex (collection: {qmd_collection})");
                    match run_qmd_reindex(&qmd_collection).await {
                        Ok(()) => {
                            tracing::info!("QMD reindex complete");
                        }
                        Err(e) => {
                            tracing::error!("QMD reindex failed: {e:#}");
                        }
                    }
                    pending_qmd_reindex = false;
                    last_export_time = None;
                }
            }
        }
    }

    Ok(())
}

/// Given a path to a JSONL file inside the projects directory, extract the project
/// name and session ID to form an `ExportJob`.
///
/// Expected layout: `<projects_dir>/<project-name>/<session-id>.jsonl`
fn extract_export_job(jsonl_path: &Path, projects_dir: &Path) -> Option<ExportJob> {
    // The path should be: projects_dir / project_name / session_id.jsonl
    let relative = jsonl_path.strip_prefix(projects_dir).ok()?;
    let components: Vec<_> = relative.components().collect();

    if components.len() != 2 {
        tracing::debug!(
            "Ignoring JSONL file not at expected depth: {}",
            jsonl_path.display()
        );
        return None;
    }

    let project_name = components[0].as_os_str().to_string_lossy().to_string();
    let session_id = jsonl_path
        .file_stem()?
        .to_string_lossy()
        .to_string();

    Some(ExportJob {
        jsonl_path: jsonl_path.to_path_buf(),
        project_name,
        session_id,
    })
}

/// Shell out to `qmd update` and `qmd embed` for the given collection.
async fn run_qmd_reindex(collection: &str) -> Result<()> {
    tracing::info!("Running: qmd update --collection {collection}");
    let update_output = tokio::process::Command::new("qmd")
        .args(["update", "--collection", collection])
        .output()
        .await
        .context("Failed to run `qmd update`")?;

    if !update_output.status.success() {
        let stderr = String::from_utf8_lossy(&update_output.stderr);
        tracing::error!("qmd update failed: {stderr}");
        anyhow::bail!("qmd update exited with status {}", update_output.status);
    }

    tracing::info!("Running: qmd embed --collection {collection}");
    let embed_output = tokio::process::Command::new("qmd")
        .args(["embed", "--collection", collection])
        .output()
        .await
        .context("Failed to run `qmd embed`")?;

    if !embed_output.status.success() {
        let stderr = String::from_utf8_lossy(&embed_output.stderr);
        tracing::error!("qmd embed failed: {stderr}");
        anyhow::bail!("qmd embed exited with status {}", embed_output.status);
    }

    Ok(())
}
