use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::Config;

/// A session summary stored as a YAML file in the summaries directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub project_path: Option<String>,
    pub date: Option<String>,
    pub summarized_at: String,
    /// mtime of the source .md file when this summary was generated.
    pub source_mtime: String,
    /// Number of messages in the session at time of summarization.
    pub message_count: usize,
    /// Rich, descriptive topics — each can be a sentence or two.
    pub topics: Vec<String>,
    /// 2-3 sentence overview of what happened.
    pub summary: String,
    /// One of: bug-fix, feature, exploration, debugging, refactoring, devops, discussion
    pub intent: String,
}

/// Queue for sessions that need summarization.
pub struct SummarizeQueue {
    inner: Mutex<VecDeque<SummarizeJob>>,
}

#[derive(Debug, Clone)]
pub struct SummarizeJob {
    pub session_id: String,
    pub md_path: PathBuf,
    pub is_update: bool,
}

impl SummarizeQueue {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
        }
    }

    pub async fn push(&self, job: SummarizeJob) {
        let mut q = self.inner.lock().await;
        // Don't enqueue duplicates
        if !q.iter().any(|j| j.session_id == job.session_id) {
            q.push_back(job);
        }
    }

    pub async fn pop(&self) -> Option<SummarizeJob> {
        self.inner.lock().await.pop_front()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

}

// ---------------------------------------------------------------------------
// Summary file I/O
// ---------------------------------------------------------------------------

/// Path to the summary file for a given session ID.
pub fn summary_path(config: &Config, session_id: &str) -> PathBuf {
    config.summaries_dir().join(format!("{session_id}.summary.yaml"))
}

/// Read an existing summary from disk, if it exists.
pub fn read_summary(config: &Config, session_id: &str) -> Option<SessionSummary> {
    let path = summary_path(config, session_id);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&content).ok()
}

/// Write a summary to disk.
pub fn write_summary(config: &Config, summary: &SessionSummary) -> Result<()> {
    let dir = config.summaries_dir();
    std::fs::create_dir_all(&dir)?;
    let path = summary_path(config, &summary.session_id);
    let yaml = serde_yaml::to_string(summary)?;
    std::fs::write(&path, yaml)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Message counting
// ---------------------------------------------------------------------------

/// Count the number of `## User` and `## Assistant` headings in a session .md file.
/// This is a cheap way to get message count without full parsing.
pub fn count_messages(md_path: &Path) -> Result<usize> {
    let content = std::fs::read_to_string(md_path)?;
    let count = content
        .lines()
        .filter(|line| *line == "## User" || *line == "## Assistant")
        .count();
    Ok(count)
}

/// Get the mtime of a file as an ISO 8601 string.
pub fn file_mtime_iso(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)?;
    let mtime = metadata.modified()?;
    let dt: DateTime<Utc> = mtime.into();
    Ok(dt.to_rfc3339())
}

/// Parse an ISO 8601 string back to SystemTime.
fn parse_iso_to_system_time(iso: &str) -> Option<SystemTime> {
    let dt = DateTime::parse_from_rfc3339(iso).ok()?;
    Some(SystemTime::from(dt))
}

// ---------------------------------------------------------------------------
// Summarization trigger logic
// ---------------------------------------------------------------------------

/// Check whether a session needs summarization and return a job if so.
pub fn check_session_needs_summary(
    config: &Config,
    session_id: &str,
    md_path: &Path,
) -> Result<Option<SummarizeJob>> {
    let current_count = count_messages(md_path)?;
    let existing = read_summary(config, session_id);

    match existing {
        None => {
            // First summary: need at least 15 messages
            if current_count >= 15 {
                Ok(Some(SummarizeJob {
                    session_id: session_id.to_string(),
                    md_path: md_path.to_path_buf(),
                    is_update: false,
                }))
            } else {
                Ok(None)
            }
        }
        Some(summary) => {
            let delta = current_count.saturating_sub(summary.message_count);

            // Trigger 1: 15+ new messages since last summary
            if delta >= 15 {
                return Ok(Some(SummarizeJob {
                    session_id: session_id.to_string(),
                    md_path: md_path.to_path_buf(),
                    is_update: true,
                }));
            }

            // Trigger 2: 5+ hours since last summary AND at least 1 new message
            if delta >= 1 {
                if let Some(summarized_time) = parse_iso_to_system_time(&summary.summarized_at) {
                    let elapsed = SystemTime::now()
                        .duration_since(summarized_time)
                        .unwrap_or_default();
                    if elapsed.as_secs() >= 5 * 3600 {
                        return Ok(Some(SummarizeJob {
                            session_id: session_id.to_string(),
                            md_path: md_path.to_path_buf(),
                            is_update: true,
                        }));
                    }
                }
            }

            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Summarization via Claude CLI
// ---------------------------------------------------------------------------

/// Generate a summary for a session by invoking `claude -p`.
pub async fn summarize_session(config: &Config, job: &SummarizeJob) -> Result<SessionSummary> {
    let md_path = &job.md_path;
    let session_id = &job.session_id;

    // Read frontmatter fields from the .md file for the summary metadata
    let md_content = std::fs::read_to_string(md_path)
        .with_context(|| format!("Failed to read session file: {}", md_path.display()))?;
    let (project_path, date) = extract_frontmatter_fields(&md_content);
    let message_count = count_messages(md_path)?;
    let source_mtime = file_mtime_iso(md_path)?;

    let raw_yaml = if job.is_update {
        // Incremental: pass previous summary + new messages
        let existing = read_summary(config, session_id)
            .ok_or_else(|| anyhow::anyhow!("Expected existing summary for update"))?;
        let delta_messages = extract_messages_after(&md_content, existing.message_count);
        run_incremental_summary(&existing, &delta_messages).await?
    } else {
        // Initial: pass file path, let Claude read it
        run_initial_summary(md_path).await?
    };

    let parsed = parse_summary_yaml(&raw_yaml)?;

    Ok(SessionSummary {
        session_id: session_id.clone(),
        project_path,
        date,
        summarized_at: Utc::now().to_rfc3339(),
        source_mtime,
        message_count,
        topics: parsed.topics,
        summary: parsed.summary,
        intent: parsed.intent,
    })
}

/// Run the initial summarization prompt — gives Claude the file path to read.
async fn run_initial_summary(md_path: &Path) -> Result<String> {
    let prompt = format!(
        "Read the session file at {} — it may be very large. \
         This is an exported Claude Code conversation in markdown format with YAML frontmatter. \
         Summarize what happened in this session.\n\n\
         Return YAML (and only YAML, no markdown fences) with these fields:\n\
         - topics: a list of rich, descriptive sentences (not single words) describing each \
         thread of work in the session. Each topic can be a couple of sentences long.\n\
         - summary: 2-3 sentences describing what happened overall\n\
         - intent: one of bug-fix, feature, exploration, debugging, refactoring, devops, discussion",
        md_path.display()
    );

    run_claude_cli(&prompt).await
}

/// Run the incremental summarization prompt — passes previous summary + delta messages.
async fn run_incremental_summary(
    existing: &SessionSummary,
    delta_messages: &str,
) -> Result<String> {
    let existing_yaml = serde_yaml::to_string(existing)?;

    let prompt = format!(
        "Here is the current summary of a Claude Code session:\n\n\
         {existing_yaml}\n\n\
         The following new messages have been added to the session:\n\n\
         {delta_messages}\n\n\
         Update the summary to incorporate the new activity.\n\n\
         Return YAML (and only YAML, no markdown fences) with these fields:\n\
         - topics: a list of rich, descriptive sentences (not single words) describing each \
         thread of work in the session. Each topic can be a couple of sentences long. \
         Include both the old topics and any new ones.\n\
         - summary: 2-3 sentences describing what happened overall (updated)\n\
         - intent: one of bug-fix, feature, exploration, debugging, refactoring, devops, discussion"
    );

    run_claude_cli(&prompt).await
}

/// Invoke `claude -p --model haiku` and return the raw output.
async fn run_claude_cli(prompt: &str) -> Result<String> {
    let output = tokio::process::Command::new("claude")
        .args(["-p", "--model", "haiku", "--output-format", "text", "--no-session-persistence"])
        .arg(prompt)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .context("Failed to run `claude` CLI. Is it installed and on PATH?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("claude CLI failed (status {}): {}", output.status, stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout)
}

// ---------------------------------------------------------------------------
// YAML parsing
// ---------------------------------------------------------------------------

/// The fields we expect back from the Claude CLI.
#[derive(Debug, Deserialize)]
struct ParsedSummaryResponse {
    topics: Vec<String>,
    summary: String,
    intent: String,
}

/// Parse the YAML response from Claude, stripping markdown fences if present.
fn parse_summary_yaml(raw: &str) -> Result<ParsedSummaryResponse> {
    let cleaned = strip_yaml_fences(raw);
    serde_yaml::from_str(&cleaned)
        .context("Failed to parse summary YAML from Claude CLI output")
}

/// Strip ```yaml ... ``` fences if present.
fn strip_yaml_fences(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with("```") {
        let lines: Vec<&str> = trimmed.lines().collect();
        if lines.len() >= 2 {
            let start = 1; // skip opening fence
            let end = if lines.last().map_or(false, |l| l.trim().starts_with("```")) {
                lines.len() - 1
            } else {
                lines.len()
            };
            return lines[start..end].join("\n");
        }
    }
    trimmed.to_string()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract project_path and date from the YAML frontmatter of a session .md.
fn extract_frontmatter_fields(md_content: &str) -> (Option<String>, Option<String>) {
    let mut project_path = None;
    let mut date = None;

    let in_frontmatter = md_content.starts_with("---");
    if !in_frontmatter {
        return (project_path, date);
    }

    for line in md_content.lines().skip(1) {
        if line.starts_with("---") {
            break;
        }
        if let Some(val) = line.strip_prefix("project_path: ") {
            project_path = Some(val.trim().trim_matches('"').to_string());
        }
        if let Some(val) = line.strip_prefix("date: ") {
            date = Some(val.trim().trim_matches('"').to_string());
        }
    }

    (project_path, date)
}

/// Extract messages after `skip_count` messages from the markdown body.
/// Messages are delimited by `## User` and `## Assistant` headings.
fn extract_messages_after(md_content: &str, skip_count: usize) -> String {
    let mut messages = Vec::new();
    let mut current_msg = String::new();
    let mut msg_index = 0;
    let mut in_body = false;

    for line in md_content.lines() {
        if !in_body {
            // Skip frontmatter
            if line == "---" {
                // Toggle: first --- starts frontmatter, second --- ends it
                if in_body {
                    // shouldn't happen but be safe
                } else {
                    // Check if we already saw opening ---
                    // Simple: just wait for blank line after second ---
                }
            }
            // We're past frontmatter once we see the first ## heading
            if line.starts_with("## User") || line.starts_with("## Assistant") {
                in_body = true;
            } else {
                continue;
            }
        }

        if line == "## User" || line == "## Assistant" {
            if !current_msg.is_empty() {
                if msg_index >= skip_count {
                    messages.push(current_msg.clone());
                }
                msg_index += 1;
                current_msg.clear();
            }
            current_msg.push_str(line);
            current_msg.push('\n');
        } else {
            current_msg.push_str(line);
            current_msg.push('\n');
        }
    }

    // Don't forget the last message
    if !current_msg.is_empty() && msg_index >= skip_count {
        messages.push(current_msg);
    }

    messages.join("\n")
}

/// Scan the sessions directory and enqueue all sessions that need summarization.
/// Returns sessions ordered newest first (by file mtime).
pub async fn enqueue_pending(config: &Config, queue: &SummarizeQueue) -> Result<usize> {
    let sessions_dir = config.export_dir();
    if !sessions_dir.is_dir() {
        return Ok(0);
    }

    // Collect (mtime, session_id, path) and sort newest first
    let mut entries: Vec<(SystemTime, String, PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let session_id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mtime = entry.metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push((mtime, session_id, path));
    }

    // Sort newest first
    entries.sort_by(|a, b| b.0.cmp(&a.0));

    let mut enqueued = 0;
    for (_mtime, session_id, path) in entries {
        match check_session_needs_summary(config, &session_id, &path) {
            Ok(Some(job)) => {
                queue.push(job).await;
                enqueued += 1;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!("Error checking session {session_id}: {e}");
            }
        }
    }

    Ok(enqueued)
}
