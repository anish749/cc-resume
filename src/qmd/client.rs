use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use thiserror::Error;
use tokio::process::Command;

use crate::config::Config;

#[derive(Debug, Error)]
pub enum QmdError {
    #[error("QMD collection '{0}' does not exist. Run `claude-resume setup` to create it and index your sessions.")]
    CollectionNotFound(String),

    #[error("QMD command `qmd {command}` failed:\n{stderr}")]
    CommandFailed { command: String, stderr: String },
}

/// A wrapper around the QMD CLI for semantic search over exported markdown sessions.
pub struct QmdClient {
    config: Config,
}

/// A single search result from QMD, enriched with frontmatter metadata.
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
    /// QMD's `collection list` output includes the path in the description line.
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
    /// Removes and recreates the collection if the path is stale.
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
            // Collection exists but points at the wrong path — remove it.
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

    /// Hybrid search via `qmd query` — query expansion + reranking.
    /// Fast (~0.3s) when QMD daemon is running, slow (~20s) cold.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.run_search("query", query, limit).await
    }

    /// Check if the QMD HTTP daemon is running.
    pub async fn is_daemon_running(&self) -> bool {
        Command::new("qmd")
            .args(["mcp", "status"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Start the QMD HTTP daemon for fast model-cached searches.
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

    async fn run_search(&self, subcommand: &str, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let collection_name = self.config.qmd_collection_name();
        let limit_str = limit.to_string();

        tracing::debug!("Running: qmd {subcommand} {query:?} -c {collection_name} -n {limit_str}");

        let output = Command::new("qmd")
            .args([
                subcommand,
                query,
                "-c",
                collection_name,
                "-n",
                &limit_str,
                "--json",
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .with_context(|| format!("Failed to run `qmd {subcommand}`"))?;

        tracing::debug!("qmd {subcommand} exited with status: {}", output.status);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr_lower = stderr.to_lowercase();
            if stderr_lower.contains("collection") && stderr_lower.contains("not found") {
                return Err(QmdError::CollectionNotFound(collection_name.into()).into());
            }
            return Err(QmdError::CommandFailed {
                command: subcommand.into(),
                stderr: stderr.into(),
            }
            .into());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        tracing::debug!("qmd output: {} bytes", stdout.len());
        let export_dir = self.config.export_dir();
        parse_search_results(&stdout, &export_dir)
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

/// Parse QMD JSON output into SearchResults, enriching each with frontmatter.
fn parse_search_results(json_str: &str, export_dir: &Path) -> Result<Vec<SearchResult>> {
    let raw_results: Vec<serde_json::Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse QMD JSON output: {e}");
            return Ok(Vec::new());
        }
    };

    let mut results = Vec::with_capacity(raw_results.len());

    for raw in &raw_results {
        let file_path = raw
            .get("file")
            .or_else(|| raw.get("displayPath"))
            .or_else(|| raw.get("path"))
            .and_then(|v| v.as_str())
            .map(|uri| resolve_qmd_uri(uri, export_dir));

        let mut result = SearchResult {
            score: raw
                .get("score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            file_path: file_path.clone(),
            ..Default::default()
        };

        if let Some(ref path) = file_path {
            match parse_frontmatter(Path::new(path)) {
                Ok(fm) => {
                    result.session_id = fm.get("session_id").cloned();
                    result.project_path = fm.get("project_path").cloned();
                    result.project_name = fm.get("project_name").cloned();
                    result.date = fm.get("date").cloned();
                    result.git_branch = fm.get("git_branch").cloned();
                    result.first_prompt = fm.get("first_prompt").cloned();
                }
                Err(e) => {
                    tracing::warn!("Failed to parse frontmatter from {path}: {e}");
                }
            }
        }

        results.push(result);
    }

    Ok(results)
}

/// Resolve a QMD URI like `qmd://claude-sessions/foo.md` to a filesystem path.
/// Falls back to returning the input as-is if it's already a real path.
fn resolve_qmd_uri(uri: &str, export_dir: &Path) -> String {
    // QMD URIs look like: qmd://collection-name/relative/path.md
    if let Some(rest) = uri.strip_prefix("qmd://") {
        // Strip the collection name (first path segment)
        if let Some((_collection, relative)) = rest.split_once('/') {
            return export_dir.join(relative).to_string_lossy().to_string();
        }
    }
    uri.to_string()
}

/// Parse YAML-style frontmatter from a markdown file.
///
/// Expects the file to start with `---`, followed by `key: value` lines,
/// closed by another `---`. Returns a map of key-value pairs.
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

            if (value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\''))
            {
                if value.len() >= 2 {
                    value = value[1..value.len() - 1].to_string();
                }
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
}
