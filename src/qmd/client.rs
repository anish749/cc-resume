use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::config::Config;

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
    /// Create a new QMD client from the application config.
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

    /// Ensure the QMD collection exists, creating it if necessary.
    ///
    /// Runs `qmd collection list` to check, then `qmd collection add` if missing.
    pub async fn ensure_collection(&self) -> Result<()> {
        let collection_name = self.config.qmd_collection_name();
        let export_dir = self.config.export_dir();

        // Make sure the export directory exists before registering it.
        std::fs::create_dir_all(export_dir)
            .with_context(|| format!("Failed to create export dir: {}", export_dir.display()))?;

        // Check if collection already exists.
        let list_output = Command::new("qmd")
            .args(["collection", "list"])
            .output()
            .await
            .context("Failed to run `qmd collection list`")?;

        if list_output.status.success() {
            let stdout = String::from_utf8_lossy(&list_output.stdout);
            // If the collection name appears in the output, it already exists.
            if stdout.contains(collection_name) {
                tracing::debug!("QMD collection '{collection_name}' already exists");
                return Ok(());
            }
        }

        // Create the collection.
        let add_output = Command::new("qmd")
            .args([
                "collection",
                "add",
                export_dir.to_str().unwrap_or_default(),
                "--name",
                collection_name,
            ])
            .output()
            .await
            .context("Failed to run `qmd collection add`")?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            anyhow::bail!("qmd collection add failed: {stderr}");
        }

        tracing::info!("Created QMD collection '{collection_name}' at {}", export_dir.display());
        Ok(())
    }

    /// Run `qmd update` to re-index documents from the filesystem.
    pub async fn update(&self) -> Result<()> {
        let output = Command::new("qmd")
            .arg("update")
            .output()
            .await
            .context("Failed to run `qmd update`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("qmd update failed: {stderr}");
        }

        tracing::debug!("QMD update completed");
        Ok(())
    }

    /// Run `qmd embed` to generate/update embeddings.
    pub async fn embed(&self) -> Result<()> {
        let output = Command::new("qmd")
            .arg("embed")
            .output()
            .await
            .context("Failed to run `qmd embed`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("qmd embed failed: {stderr}");
        }

        tracing::debug!("QMD embed completed");
        Ok(())
    }

    /// Run a hybrid search (semantic + keyword) via `qmd query`.
    ///
    /// Returns results enriched with frontmatter metadata from each matched file.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.run_search_command("query", query, limit).await
    }

    /// Common implementation for search commands (`query`, `search`, `vsearch`).
    async fn run_search_command(
        &self,
        subcommand: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let collection_name = self.config.qmd_collection_name();
        let limit_str = limit.to_string();

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
            .output()
            .await
            .with_context(|| format!("Failed to run `qmd {subcommand}`"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("qmd {subcommand} failed: {stderr}");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse the JSON array of results.
        let raw_results: Vec<serde_json::Value> = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to parse QMD JSON output: {e}");
                tracing::debug!("Raw output: {stdout}");
                return Ok(Vec::new());
            }
        };

        let mut results = Vec::with_capacity(raw_results.len());

        for raw in &raw_results {
            let file_path = raw
                .get("displayPath")
                .or_else(|| raw.get("path"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let mut result = SearchResult {
                score: raw
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                file_path: file_path.clone(),
                ..Default::default()
            };

            // Enrich with frontmatter from the actual file.
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
}

/// Parse YAML-style frontmatter from a markdown file.
///
/// Expects the file to start with `---`, followed by `key: value` lines,
/// closed by another `---`. Returns a map of key-value pairs.
/// Uses simple string splitting rather than a full YAML parser.
fn parse_frontmatter(path: &Path) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let mut map = HashMap::new();
    let mut lines = content.lines();

    // First line must be "---"
    match lines.next() {
        Some(line) if line.trim() == "---" => {}
        _ => return Ok(map), // No frontmatter
    }

    for line in lines {
        let trimmed = line.trim();

        // End of frontmatter block
        if trimmed == "---" {
            break;
        }

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Parse "key: value" — split on first ':'
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_string();
            let mut value = value.trim().to_string();

            // Strip surrounding quotes if present
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
        assert!(fm.get("git_branch").is_none()); // empty value skipped

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
