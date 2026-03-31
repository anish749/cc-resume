//! Indexing pipeline: the complete workflow for keeping search up to date.
//!
//! Runs as a single pass with these steps:
//!   1. **Scan** — find stale sessions (JSONL mtime > markdown mtime)
//!   2. **Export** — convert stale JSONLs to markdown (preserving existing AI summaries)
//!   3. **Summarize** — generate/update AI summaries for sessions that need them
//!   4. **Inject** — write summaries into markdown frontmatter
//!   5. **Reindex** — `qmd update + embed` so search picks up all changes

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;

/// A session whose source JSONL is newer than its exported markdown.
struct StaleSession {
    jsonl_path: PathBuf,
    output_path: PathBuf,
    project_name: String,
    session_id: String,
}

/// Run the full indexing pipeline. Returns the number of sessions exported.
pub async fn run(config: &Config) -> Result<usize> {
    let projects_dirs = config.claude_projects_dirs();
    let export_dir = config.export_dir();

    // Step 1: Scan for stale sessions across all registered source dirs.
    let mut stale = Vec::new();
    let mut seen_sessions = std::collections::HashSet::new();
    for projects_dir in &projects_dirs {
        for session in scan_stale(projects_dir, &export_dir) {
            if seen_sessions.insert(session.session_id.clone()) {
                stale.push(session);
            }
        }
    }
    if stale.is_empty() {
        tracing::debug!("No stale sessions found (scanned {} source dirs)", projects_dirs.len());
        return Ok(0);
    }
    tracing::info!("Found {} stale sessions across {} source dirs", stale.len(), projects_dirs.len());

    // Step 2: Export stale JSONLs to markdown (preserves existing AI summaries).
    let exported = export(config, &stale);
    if exported == 0 {
        return Ok(0);
    }
    tracing::info!("Exported {exported}/{} sessions", stale.len());

    // Step 3 + 4: Summarize sessions that need it and inject into frontmatter.
    summarize_and_inject(config).await;

    // Step 5: Reindex QMD.
    tracing::info!("Triggering QMD reindex");
    if let Err(e) = reindex_qmd().await {
        tracing::warn!("QMD reindex failed: {e}");
    }

    Ok(exported)
}

// ---------------------------------------------------------------------------
// Step 1: Scan
// ---------------------------------------------------------------------------

/// Find all sessions whose JSONL mtime is newer than their markdown mtime
/// (or that have never been exported).
fn scan_stale(projects_dir: &Path, export_dir: &Path) -> Vec<StaleSession> {
    let mut stale = Vec::new();

    let project_entries = match std::fs::read_dir(projects_dir) {
        Ok(entries) => entries,
        Err(_) => return stale,
    };

    for project_entry in project_entries.filter_map(|e| e.ok()) {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let project_name = project_entry.file_name().to_string_lossy().to_string();

        let session_entries = match std::fs::read_dir(&project_path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for session_entry in session_entries.filter_map(|e| e.ok()) {
            let jsonl_path = session_entry.path();
            if jsonl_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let session_id = match jsonl_path.file_stem() {
                Some(s) => s.to_string_lossy().to_string(),
                None => continue,
            };

            let output_path = export_dir.join(format!("{session_id}.md"));

            let jsonl_mtime = match std::fs::metadata(&jsonl_path).and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue,
            };

            let is_stale = match std::fs::metadata(&output_path).and_then(|m| m.modified()) {
                Ok(md_mtime) => jsonl_mtime > md_mtime,
                Err(_) => true, // no markdown yet
            };

            if is_stale {
                stale.push(StaleSession {
                    jsonl_path,
                    output_path,
                    project_name: project_name.clone(),
                    session_id,
                });
            }
        }
    }

    stale
}

// ---------------------------------------------------------------------------
// Step 2: Export
// ---------------------------------------------------------------------------

/// Export stale sessions to markdown. Returns the number successfully exported.
fn export(config: &Config, stale: &[StaleSession]) -> usize {
    std::fs::create_dir_all(config.export_dir()).ok();

    let mut exported = 0;
    for session in stale {
        match crate::exporter::export_session(
            &session.jsonl_path,
            &session.output_path,
            &session.project_name,
            &session.session_id,
        ) {
            Ok(_) => {
                tracing::debug!("Exported {}", session.session_id);
                exported += 1;
            }
            Err(e) => {
                tracing::warn!("Failed to export {}: {e}", session.session_id);
            }
        }
    }
    exported
}

// ---------------------------------------------------------------------------
// Step 3 + 4: Summarize and inject
// ---------------------------------------------------------------------------

/// Find sessions that need summarization, run Claude on each, and inject
/// the results into the markdown frontmatter.
async fn summarize_and_inject(config: &Config) {
    use crate::summarizer;

    let queue = summarizer::SummarizeQueue::new();
    let enqueued = match summarizer::enqueue_pending(config, &queue).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("Failed to scan for sessions needing summarization: {e}");
            return;
        }
    };

    if enqueued == 0 {
        return;
    }

    tracing::info!("Summarizing {enqueued} sessions");
    let mut done = 0;
    while let Some(job) = queue.pop().await {
        match summarizer::summarize_session(config, &job).await {
            Ok(summary) => {
                if let Err(e) = summarizer::write_summary(config, &summary) {
                    tracing::warn!("Failed to write summary for {}: {e}", job.session_id);
                } else {
                    done += 1;
                }
            }
            Err(e) => {
                tracing::warn!("Failed to summarize {}: {e}", job.session_id);
            }
        }
    }
    if done > 0 {
        tracing::info!("Summarized {done}/{enqueued} sessions");
    }
}

// ---------------------------------------------------------------------------
// Step 5: Reindex
// ---------------------------------------------------------------------------

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
