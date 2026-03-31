use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;

const DEFAULT_CLAUDE_DIR: &str = ".claude";
const DATA_DIR_NAME: &str = ".ccresume";
const QMD_COLLECTION: &str = "claude-sessions";
const SOURCES_FILE: &str = "sources.txt";

/// Application configuration, resolved from environment and defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// Root Claude config directory (e.g., ~/.claude)
    claude_config_dir: PathBuf,
    /// Our own data directory, always ~/.ccresume
    data_dir: PathBuf,
}

impl Config {
    /// Load configuration, respecting CLAUDE_CONFIG_DIR env var.
    /// Registers the current Claude config dir in the persistent sources list.
    pub fn load() -> Result<Self> {
        let claude_config_dir = Self::resolve_claude_dir()?;

        // Always place .ccresume in the user's home directory so that data
        // stays in one place regardless of CLAUDE_CONFIG_DIR (which may vary
        // per-project via direnv). CLAUDE_CONFIG_DIR only affects where we
        // read source session JSONLs from.
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let data_dir = home.join(DATA_DIR_NAME);

        let config = Self {
            claude_config_dir,
            data_dir,
        };

        // Register this config dir so future pipeline runs scan it.
        if let Err(e) = config.register_source(&config.claude_config_dir) {
            tracing::warn!("Failed to register source dir: {e}");
        }

        Ok(config)
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

    /// All registered Claude projects directories to scan for session JSONLs.
    /// Always includes ~/.claude/projects/ plus any additional dirs registered
    /// via previous runs with different CLAUDE_CONFIG_DIR values.
    pub fn claude_projects_dirs(&self) -> Vec<PathBuf> {
        let sources = self.load_sources();
        sources
            .into_iter()
            .map(|dir| PathBuf::from(dir).join("projects"))
            .filter(|p| p.is_dir())
            .collect()
    }

    /// Path to the export directory (markdown files for QMD).
    pub fn export_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    /// QMD collection name.
    pub fn qmd_collection_name(&self) -> &str {
        QMD_COLLECTION
    }

    /// Path to the daemon PID file.
    pub fn daemon_pid_file(&self) -> PathBuf {
        self.data_dir.join("daemon.pid")
    }

    /// Path to the daemon log file.
    pub fn daemon_log_file(&self) -> PathBuf {
        self.data_dir.join("daemon.log")
    }

    /// Path to the summaries directory (YAML summary files).
    pub fn summaries_dir(&self) -> PathBuf {
        self.data_dir.join("summaries")
    }

    /// Path to the indexing lock file (present while pipeline is running).
    pub fn indexing_lock_file(&self) -> PathBuf {
        self.data_dir.join("indexing")
    }

    /// Whether the indexing pipeline is currently running.
    pub fn is_indexing(&self) -> bool {
        self.indexing_lock_file().exists()
    }

    fn sources_file(&self) -> PathBuf {
        self.data_dir.join(SOURCES_FILE)
    }

    /// Register a Claude config directory in the persistent sources list.
    fn register_source(&self, dir: &PathBuf) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let mut sources = self.load_sources();
        let canonical = dir.to_string_lossy().to_string();
        if sources.insert(canonical) {
            let content: Vec<&str> = sources.iter().map(|s| s.as_str()).collect();
            std::fs::write(self.sources_file(), content.join("\n") + "\n")?;
        }
        Ok(())
    }

    /// Load the set of registered source directories.
    fn load_sources(&self) -> BTreeSet<String> {
        let mut sources = BTreeSet::new();
        // Always include the default ~/.claude
        if let Some(home) = dirs::home_dir() {
            let default = home.join(DEFAULT_CLAUDE_DIR);
            if default.is_dir() {
                sources.insert(default.to_string_lossy().to_string());
            }
        }
        // Load additional dirs from the sources file.
        if let Ok(content) = std::fs::read_to_string(self.sources_file()) {
            for line in content.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    sources.insert(line.to_string());
                }
            }
        }
        sources
    }
}
