use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use thiserror::Error;
use tokio::process::Command;

use crate::config::Config;

const QMD_MCP_URL: &str = "http://localhost:8181/mcp";

#[derive(Debug, Error)]
pub enum QmdError {
    #[error("QMD collection '{0}' does not exist. Run `claude-resume setup` to create it and index your sessions.")]
    CollectionNotFound(String),

    #[error("QMD command `qmd {command}` failed:\n{stderr}")]
    CommandFailed { command: String, stderr: String },

    #[error("QMD daemon is not running. Run `claude-resume daemon start` first.")]
    DaemonNotRunning,

    #[error("QMD search failed: {0}")]
    SearchFailed(String),
}

/// A wrapper around the QMD MCP daemon for semantic search.
pub struct QmdClient {
    config: Config,
    http: reqwest::Client,
    /// MCP session ID, lazily initialized on first search.
    session_id: Mutex<Option<String>>,
}

/// A single search result enriched with frontmatter metadata.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub score: f64,
    pub file_path: Option<String>,
    pub session_id: Option<String>,
    pub project_path: Option<String>,
    pub project_name: Option<String>,
    pub date: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
}

impl QmdClient {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            http: reqwest::Client::new(),
            session_id: Mutex::new(None),
        }
    }

    /// Check whether the `qmd` CLI is available on PATH.
    pub fn is_installed(&self) -> bool {
        std::process::Command::new("qmd")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Check whether the collection exists by listing collections.
    async fn collection_exists(&self) -> Result<bool> {
        let collection_name = self.config.qmd_collection_name();
        let output = Command::new("qmd")
            .args(["collection", "list"])
            .output()
            .await
            .context("Failed to run `qmd collection list`")?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains(collection_name))
    }

    /// Check whether the collection points at the expected export directory.
    async fn collection_path_matches(&self) -> Result<bool> {
        let export_dir = self.config.export_dir();
        let output = Command::new("qmd")
            .args(["collection", "list"])
            .output()
            .await
            .context("Failed to run `qmd collection list`")?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let export_dir_str = export_dir.to_string_lossy();
        Ok(stdout.contains(export_dir_str.as_ref()))
    }

    /// Ensure the QMD collection exists and points at the correct export directory.
    pub async fn ensure_collection(&self) -> Result<()> {
        let collection_name = self.config.qmd_collection_name();
        let export_dir = self.config.export_dir();

        std::fs::create_dir_all(&export_dir)
            .with_context(|| format!("Failed to create export dir: {}", export_dir.display()))?;

        if self.collection_exists().await? {
            if self.collection_path_matches().await? {
                tracing::debug!("QMD collection '{collection_name}' already exists with correct path");
                return Ok(());
            }
            tracing::info!("QMD collection '{collection_name}' points at wrong path, recreating");
            run_qmd_command(&["collection", "remove", collection_name]).await?;
        }

        let status = Command::new("qmd")
            .args([
                "collection",
                "add",
                export_dir.to_str().unwrap_or_default(),
                "--name",
                collection_name,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .await
            .context("Failed to run `qmd collection add`")?;

        if !status.success() {
            return Err(QmdError::CommandFailed {
                command: "collection add".into(),
                stderr: format!("exited with status {status}"),
            }
            .into());
        }

        tracing::info!("Created QMD collection '{collection_name}' at {}", export_dir.display());
        Ok(())
    }

    /// Run `qmd update` to re-index documents from the filesystem.
    pub async fn update(&self) -> Result<()> {
        run_qmd_command(&["update"]).await
    }

    /// Run `qmd embed` to generate/update embeddings.
    pub async fn embed(&self) -> Result<()> {
        run_qmd_command(&["embed"]).await
    }

    /// Check if the QMD HTTP daemon is running.
    pub async fn is_daemon_running(&self) -> bool {
        self.http
            .post(QMD_MCP_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "ping",
            }))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .is_ok()
    }

    /// Start the QMD HTTP daemon.
    pub async fn start_daemon(&self) -> Result<()> {
        if self.is_daemon_running().await {
            return Ok(());
        }
        let status = Command::new("qmd")
            .args(["mcp", "--http", "--daemon"])
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .stdin(std::process::Stdio::null())
            .status()
            .await
            .context("Failed to start QMD daemon")?;

        if !status.success() {
            anyhow::bail!("Failed to start QMD daemon");
        }
        Ok(())
    }

    /// Initialize an MCP session with the daemon, returning the session ID.
    async fn init_mcp_session(&self) -> Result<String> {
        let resp = self
            .http
            .post(QMD_MCP_URL)
            // Init requires text/event-stream to get session ID in response headers
            .header("Accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "claude-resume", "version": "0.1.0"}
                }
            }))
            .send()
            .await
            .context("Failed to connect to QMD daemon")?;

        let session_id = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("QMD daemon did not return a session ID"))?;

        // Consume the response body so the connection can be cleanly reused.
        // Without this, the chunked body stays in the TCP buffer and the next
        // request on this pooled connection stalls for seconds.
        let _ = resp.bytes().await;

        tracing::debug!("MCP session initialized: {session_id}");
        Ok(session_id)
    }

    /// Get or create an MCP session ID.
    async fn get_session_id(&self) -> Result<String> {
        {
            let guard = self.session_id.lock().unwrap();
            if let Some(ref id) = *guard {
                return Ok(id.clone());
            }
        }

        let id = self.init_mcp_session().await?;
        {
            let mut guard = self.session_id.lock().unwrap();
            *guard = Some(id.clone());
        }
        Ok(id)
    }

    /// Search via the QMD MCP daemon HTTP endpoint.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let collection_name = self.config.qmd_collection_name();
        let t_session = std::time::Instant::now();
        let session_id = self.get_session_id().await.map_err(|e| {
            tracing::warn!("Failed to get MCP session: {e}");
            QmdError::DaemonNotRunning
        })?;
        tracing::debug!("get_session_id: {:?}", t_session.elapsed());

        tracing::debug!("MCP search: query={query:?} collection={collection_name} limit={limit}");

        let t_http = std::time::Instant::now();
        let resp = self
            .http
            .post(QMD_MCP_URL)
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Session-Id", &session_id)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "query",
                    "arguments": {
                        "searches": [
                            {"type": "lex", "query": query},
                            {"type": "vec", "query": query}
                        ],
                        "intent": query,
                        "collection": collection_name,
                        "limit": limit
                    }
                }
            }))
            .send()
            .await
            .map_err(|e| QmdError::SearchFailed(format!("HTTP request failed: {e}")))?;

        tracing::debug!("HTTP send: {:?}", t_http.elapsed());

        let t_body = std::time::Instant::now();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| QmdError::SearchFailed(format!("Failed to parse response: {e}")))?;
        tracing::debug!("HTTP body read: {:?}", t_body.elapsed());

        // Check for JSON-RPC error
        if let Some(error) = body.get("error") {
            let msg = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");

            // Session might have expired — clear it and retry once
            if msg.contains("session") || msg.contains("Session") {
                tracing::debug!("MCP session expired, reinitializing");
                {
                    let mut guard = self.session_id.lock().unwrap();
                    *guard = None;
                }
                // Don't retry here to avoid recursion — let the caller retry
                return Err(QmdError::SearchFailed(format!("Session expired: {msg}")).into());
            }

            let msg_lower = msg.to_lowercase();
            if msg_lower.contains("collection") && msg_lower.contains("not found") {
                return Err(QmdError::CollectionNotFound(
                    self.config.qmd_collection_name().to_string(),
                ).into());
            }
            return Err(QmdError::SearchFailed(msg.to_string()).into());
        }

        // Extract results from MCP response.
        // The response is: result.content[0].text = "Found N results...\n\n#docid score path - title\n..."
        let text = body
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let export_dir = self.config.export_dir();
        parse_mcp_results(text, &export_dir)
    }
}

/// Run a QMD command with stdout/stderr inherited so the user sees progress.
async fn run_qmd_command(args: &[&str]) -> Result<()> {
    let status = Command::new("qmd")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("Failed to run `qmd {}`", args.join(" ")))?;

    if !status.success() {
        return Err(QmdError::CommandFailed {
            command: args.join(" "),
            stderr: format!("exited with status {status}"),
        }
        .into());
    }

    Ok(())
}

/// Parse the text output from QMD MCP query tool.
/// Format: "Found N results for "query":\n\n#docid 92% collection/file.md - Title\n..."
fn parse_mcp_results(text: &str, export_dir: &Path) -> Result<Vec<SearchResult>> {
    let mut results = Vec::new();

    for line in text.lines() {
        // Lines look like: #docid 92% claude-sessions/file.md - Title
        let line = line.trim();
        if !line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.splitn(4, ' ').collect();
        if parts.len() < 3 {
            continue;
        }

        // parts[0] = "#docid", parts[1] = "92%", parts[2] = "collection/file.md", parts[3] = "- Title"
        let score_str = parts[1].trim_end_matches('%');
        let score = score_str.parse::<f64>().unwrap_or(0.0) / 100.0;

        let qmd_path = parts[2];
        // The path is like "claude-sessions/uuid.md" — resolve to filesystem
        let file_path = export_dir
            .join(
                qmd_path
                    .strip_prefix("claude-sessions/")
                    .unwrap_or(qmd_path),
            )
            .to_string_lossy()
            .to_string();

        let mut result = SearchResult {
            score,
            file_path: Some(file_path.clone()),
            ..Default::default()
        };

        // Enrich with frontmatter
        if Path::new(&file_path).exists() {
            match parse_frontmatter(Path::new(&file_path)) {
                Ok(fm) => {
                    result.session_id = fm.get("session_id").cloned();
                    result.project_path = fm.get("project_path").cloned();
                    result.project_name = fm.get("project_name").cloned();
                    result.date = fm.get("date").cloned();
                    result.git_branch = fm.get("git_branch").cloned();
                    result.first_prompt = fm.get("first_prompt").cloned();
                }
                Err(e) => {
                    tracing::warn!("Failed to parse frontmatter from {file_path}: {e}");
                }
            }
        }

        results.push(result);
    }

    Ok(results)
}

/// Parse YAML-style frontmatter from a markdown file.
fn parse_frontmatter(path: &Path) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let mut map = HashMap::new();
    let mut lines = content.lines();

    match lines.next() {
        Some(line) if line.trim() == "---" => {}
        _ => return Ok(map),
    }

    for line in lines {
        let trimmed = line.trim();

        if trimmed == "---" {
            break;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_string();
            let mut value = value.trim().to_string();

            let is_quoted = (value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\''));
            if is_quoted && value.len() >= 2 {
                value = value[1..value.len() - 1].to_string();
            }

            if !key.is_empty() && !value.is_empty() {
                map.insert(key, value);
            }
        }
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_frontmatter_basic() {
        let dir = std::env::temp_dir().join("qmd_client_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_basic.md");

        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"---
session_id: abc-123
project_path: /Users/anish/git/foo
project_name: foo
date: 2025-03-15
git_branch: main
first_prompt: "Fix the bug in auth"
---

# Session content here"#
        )
        .unwrap();

        let fm = parse_frontmatter(&path).unwrap();
        assert_eq!(fm.get("session_id").unwrap(), "abc-123");
        assert_eq!(fm.get("project_path").unwrap(), "/Users/anish/git/foo");
        assert_eq!(fm.get("project_name").unwrap(), "foo");
        assert_eq!(fm.get("date").unwrap(), "2025-03-15");
        assert_eq!(fm.get("git_branch").unwrap(), "main");
        assert_eq!(fm.get("first_prompt").unwrap(), "Fix the bug in auth");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let dir = std::env::temp_dir().join("qmd_client_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_no_fm.md");

        std::fs::write(&path, "# Just a heading\n\nSome content.\n").unwrap();

        let fm = parse_frontmatter(&path).unwrap();
        assert!(fm.is_empty());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_frontmatter_empty_values_skipped() {
        let dir = std::env::temp_dir().join("qmd_client_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_empty.md");

        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"---
session_id: abc-123
git_branch:
---"#
        )
        .unwrap();

        let fm = parse_frontmatter(&path).unwrap();
        assert_eq!(fm.get("session_id").unwrap(), "abc-123");
        assert!(fm.get("git_branch").is_none());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_frontmatter_single_quotes() {
        let dir = std::env::temp_dir().join("qmd_client_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_quotes.md");

        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"---
first_prompt: 'Build the TUI'
---"#
        )
        .unwrap();

        let fm = parse_frontmatter(&path).unwrap();
        assert_eq!(fm.get("first_prompt").unwrap(), "Build the TUI");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_mcp_results() {
        let text = r#"Found 3 results for "database":

#6ada51 92% claude-sessions/abc-123.md - Session: 2026-03-03 (abc123)
#ad31cc 50% claude-sessions/def-456.md - Session: 2026-03-28 (def456)
#720521 44% claude-sessions/ghi-789.md - Session: 2026-03-27 (ghi789)"#;

        let dir = std::env::temp_dir().join("mcp_parse_test");
        let results = parse_mcp_results(text, &dir).unwrap();
        assert_eq!(results.len(), 3);
        assert!((results[0].score - 0.92).abs() < 0.01);
        assert!((results[1].score - 0.50).abs() < 0.01);
        assert!((results[2].score - 0.44).abs() < 0.01);
        assert!(results[0].file_path.as_ref().unwrap().contains("abc-123.md"));
    }

}
