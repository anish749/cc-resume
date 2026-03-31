use std::process::Command;

use anyhow::Result;

use crate::qmd::SearchResult;

/// Resume a Claude Code session by its search result.
/// Reads the session_id and project_path from the result's frontmatter,
/// then invokes `claude --resume <session_id>` in the correct directory.
pub fn resume_session(result: &SearchResult) -> Result<()> {
    let file = result.file_path.as_deref().unwrap_or("<unknown>");

    let session_id = result
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("No session_id in frontmatter of {file}"))?;

    let project_path = result
        .project_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("No project_path in frontmatter of {file}"))?;

    // Decode the project name back to a path.
    // Claude stores projects as "-Users-anish-git-foo" → "/Users/anish/git/foo"
    let cwd = decode_project_path(project_path);

    tracing::info!("Resuming session {session_id} in {cwd}");

    let status = Command::new("claude")
        .arg("--resume")
        .arg(session_id)
        .current_dir(&cwd)
        .status()?;

    if !status.success() {
        anyhow::bail!("claude --resume exited with status: {status}");
    }

    Ok(())
}

/// Decode a Claude project directory name back to an absolute path.
/// "-Users-anish-git-foo" → "/Users/anish/git/foo"
fn decode_project_path(encoded: &str) -> String {
    // The encoding replaces "/" with "-", and the leading "-" represents "/"
    // We reconstruct by replacing leading "-" with "/" and subsequent "-" with "/"
    // But this is ambiguous if directory names contain dashes.
    // For now, use the simple heuristic: leading dash = root slash, then split on dashes
    // and try to find the longest valid path.
    //
    // A more robust approach: store the original path in frontmatter.
    // We do this — the project_path field in frontmatter contains the decoded path.
    // This function is a fallback for when that's not available.
    if encoded.starts_with('-') {
        encoded.replacen('-', "/", 1).replace('-', "/")
    } else {
        encoded.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_project_path() {
        assert_eq!(
            decode_project_path("-Users-anish-git-foo"),
            "/Users/anish/git/foo"
        );
    }
}
