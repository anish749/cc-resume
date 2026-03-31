pub(crate) mod markdown;
mod parser;

use std::path::Path;

use anyhow::Result;

use crate::config::Config;

/// Stats from an export run.
#[derive(Debug, Default)]
pub struct ExportStats {
    pub exported: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Export all sessions from all registered Claude projects directories.
/// If `full` is true, re-export everything. Otherwise, skip already-exported sessions.
pub async fn export_all(config: &Config, full: bool) -> Result<ExportStats> {
    let projects_dirs = config.claude_projects_dirs();
    let export_dir = config.export_dir();

    std::fs::create_dir_all(&export_dir)?;

    let mut stats = ExportStats::default();

    if projects_dirs.is_empty() {
        anyhow::bail!("No Claude projects directories found");
    }

    for projects_dir in &projects_dirs {
        for project_entry in std::fs::read_dir(projects_dir)? {
            let project_entry = project_entry?;
            let project_path = project_entry.path();

            if !project_path.is_dir() {
                continue;
            }

            let project_name = project_entry
                .file_name()
                .to_string_lossy()
                .to_string();

            for session_entry in std::fs::read_dir(&project_path)? {
                let session_entry = session_entry?;
                let session_file = session_entry.path();

                if session_file.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }

                let session_id = session_file
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                let output_file = export_dir.join(format!("{session_id}.md"));

                if !full && output_file.exists() {
                    stats.skipped += 1;
                    continue;
                }

                match export_session(&session_file, &output_file, &project_name, &session_id) {
                    Ok(_) => stats.exported += 1,
                    Err(e) => {
                        tracing::warn!("Failed to export {}: {e}", session_file.display());
                        stats.errors += 1;
                    }
                }
            }
        }
    }

    Ok(stats)
}

/// Export a single session, replacing the output file if it already exists.
/// Preserves any existing AI summary fields from the previous markdown.
pub fn export_session(
    jsonl_path: &Path,
    output_path: &Path,
    project_name: &str,
    session_id: &str,
) -> Result<()> {
    let parsed = parser::parse_session(jsonl_path)?;

    if parsed.messages.is_empty() {
        return Ok(());
    }

    // Read existing AI summary fields before overwriting.
    let existing_ai = std::fs::read_to_string(output_path)
        .ok()
        .and_then(|content| markdown::SessionDocument::parse(&content))
        .map(|doc| (doc.frontmatter.ai_summary, doc.frontmatter.ai_topics, doc.frontmatter.ai_intent));

    let metadata = parser::extract_metadata(&parsed, project_name, session_id);
    let mut md = markdown::render(&metadata, &parsed.messages);

    // Restore AI summary fields into the freshly rendered markdown.
    if let Some((ai_summary, ai_topics, ai_intent)) = existing_ai {
        if ai_summary.is_some() || ai_topics.is_some() || ai_intent.is_some() {
            if let Some(mut doc) = markdown::SessionDocument::parse(&md) {
                doc.frontmatter.ai_summary = ai_summary;
                doc.frontmatter.ai_topics = ai_topics;
                doc.frontmatter.ai_intent = ai_intent;
                md = doc.render();
            }
        }
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(output_path, md)?;
    Ok(())
}
