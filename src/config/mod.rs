use std::path::PathBuf;

use anyhow::Result;

const DEFAULT_CLAUDE_DIR: &str = ".claude";
const EXPORT_DIR_NAME: &str = "claude-resume";
const QMD_COLLECTION: &str = "claude-sessions";

/// Application configuration, resolved from environment and defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// Root Claude config directory (e.g., ~/.claude)
    claude_config_dir: PathBuf,
    /// Where exported markdown sessions live
    export_dir: PathBuf,
}

impl Config {
    /// Load configuration, respecting CLAUDE_CONFIG_DIR env var.
    pub fn load() -> Result<Self> {
        let claude_config_dir = Self::resolve_claude_dir()?;
        let export_dir = claude_config_dir.join(EXPORT_DIR_NAME).join("sessions");

        Ok(Self {
            claude_config_dir,
            export_dir,
        })
    }

    fn resolve_claude_dir() -> Result<PathBuf> {
        if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
            let path = PathBuf::from(dir);
            if path.is_dir() {
                return Ok(path);
            }
            anyhow::bail!(
                "CLAUDE_CONFIG_DIR is set to '{}' but it doesn't exist",
                path.display()
            );
        }

        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let default = home.join(DEFAULT_CLAUDE_DIR);

        if !default.is_dir() {
            anyhow::bail!(
                "Claude config directory not found at {}. Is Claude Code installed?",
                default.display()
            );
        }

        Ok(default)
    }

    /// Path to the Claude projects directory (contains session JSONL files).
    pub fn claude_projects_dir(&self) -> PathBuf {
        self.claude_config_dir.join("projects")
    }

    /// Path to the export directory (markdown files for QMD).
    pub fn export_dir(&self) -> &PathBuf {
        &self.export_dir
    }

    /// QMD collection name.
    pub fn qmd_collection_name(&self) -> &str {
        QMD_COLLECTION
    }

    /// Path to the daemon PID file.
    pub fn daemon_pid_file(&self) -> PathBuf {
        self.claude_config_dir
            .join(EXPORT_DIR_NAME)
            .join("daemon.pid")
    }

    /// Path to the daemon log file.
    pub fn daemon_log_file(&self) -> PathBuf {
        self.claude_config_dir
            .join(EXPORT_DIR_NAME)
            .join("daemon.log")
    }
}
