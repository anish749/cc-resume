use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

/// Role of a session message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

/// A single user or assistant message extracted from a session JSONL.
#[derive(Debug, Clone)]
pub struct SessionMessage {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: Option<String>,
}

/// Metadata derived from a parsed session.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub session_id: String,
    pub project_name: String,
    pub project_path: String,
    pub date: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub files_touched: Vec<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

/// Intermediate parsed data from a JSONL file, carrying both the displayable
/// messages and the raw fields needed to build metadata.
#[derive(Debug, Clone)]
pub struct ParsedSession {
    pub messages: Vec<SessionMessage>,
    /// The `cwd` field from the first message, if any.
    pub cwd: Option<String>,
    /// The `gitBranch` field from the first message that has one.
    pub git_branch: Option<String>,
    /// Unique file paths mentioned in tool_use content blocks.
    pub files_touched: Vec<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read a JSONL file and extract user + assistant messages along with raw
/// metadata fields needed later by `extract_metadata`.
pub fn parse_session(path: &Path) -> Result<ParsedSession> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut messages: Vec<SessionMessage> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut files: BTreeSet<String> = BTreeSet::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines
        };

        // Extract cwd from the first line that has it.
        if cwd.is_none() {
            if let Some(s) = value.get("cwd").and_then(Value::as_str) {
                if !s.is_empty() {
                    cwd = Some(s.to_string());
                }
            }
        }

        // Extract gitBranch from the first line that has it.
        if git_branch.is_none() {
            if let Some(s) = value.get("gitBranch").and_then(Value::as_str) {
                if !s.is_empty() {
                    git_branch = Some(s.to_string());
                }
            }
        }

        let msg_type = match value.get("type").and_then(Value::as_str) {
            Some(t) => t,
            None => continue,
        };

        match msg_type {
            "user" => {
                let timestamp = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(String::from);

                let content = extract_user_content(&value);
                if !content.is_empty() {
                    messages.push(SessionMessage {
                        role: MessageRole::User,
                        content,
                        timestamp,
                    });
                }
            }
            "assistant" => {
                let timestamp = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(String::from);

                let (text, touched) = extract_assistant_content(&value);
                for f in touched {
                    files.insert(f);
                }
                if !text.is_empty() {
                    messages.push(SessionMessage {
                        role: MessageRole::Assistant,
                        content: text,
                        timestamp,
                    });
                }
            }
            // Skip everything else: system, progress, file-history-snapshot, etc.
            _ => {}
        }
    }

    Ok(ParsedSession {
        messages,
        cwd,
        git_branch,
        files_touched: files.into_iter().collect(),
    })
}

/// Build session metadata from the parsed session data.
pub fn extract_metadata(
    parsed: &ParsedSession,
    project_name: &str,
    session_id: &str,
) -> SessionMetadata {
    let messages = &parsed.messages;

    // Timestamps: first and last across all messages.
    let started_at = messages
        .iter()
        .filter_map(|m| m.timestamp.as_deref())
        .next()
        .map(String::from);

    let ended_at = messages
        .iter()
        .rev()
        .filter_map(|m| m.timestamp.as_deref())
        .next()
        .map(String::from);

    // Date: YYYY-MM-DD from the first timestamp.
    let date = started_at
        .as_deref()
        .and_then(|ts| ts.get(..10))
        .map(String::from);

    // First user prompt, truncated to 200 chars.
    let first_prompt = messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .map(|m| truncate(&m.content, 200));

    // Project path: prefer the cwd extracted from the JSONL; fall back to
    // decoding the directory name (e.g. "-Users-anish-git-foo" -> "/Users/anish/git/foo").
    let project_path = parsed
        .cwd
        .clone()
        .unwrap_or_else(|| decode_project_dir(project_name));

    SessionMetadata {
        session_id: session_id.to_string(),
        project_name: project_name.to_string(),
        project_path,
        date,
        git_branch: parsed.git_branch.clone(),
        first_prompt,
        files_touched: parsed.files_touched.clone(),
        started_at,
        ended_at,
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Extract the text content from a user message.
///
/// `message.content` can be either a plain string or an array of content
/// blocks.
fn extract_user_content(value: &Value) -> String {
    let message = match value.get("message") {
        Some(m) => m,
        None => return String::new(),
    };

    let content = match message.get("content") {
        Some(c) => c,
        None => return String::new(),
    };

    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut parts: Vec<String> = Vec::new();
            for block in arr {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(s) = block.get("text").and_then(Value::as_str) {
                        parts.push(s.to_string());
                    }
                }
            }
            parts.join("\n\n")
        }
        _ => String::new(),
    }
}

/// Extract text-only content from an assistant message, plus any file paths
/// found in tool_use blocks.
///
/// Returns (text_content, files_touched).
fn extract_assistant_content(value: &Value) -> (String, Vec<String>) {
    let message = match value.get("message") {
        Some(m) => m,
        None => return (String::new(), Vec::new()),
    };

    let content = match message.get("content") {
        Some(c) => c,
        None => return (String::new(), Vec::new()),
    };

    let mut text_parts: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    if let Value::Array(arr) = content {
        for block in arr {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
            match block_type {
                "text" => {
                    if let Some(s) = block.get("text").and_then(Value::as_str) {
                        if !s.is_empty() {
                            text_parts.push(s.to_string());
                        }
                    }
                }
                "tool_use" => {
                    // Extract file paths from the tool input object.
                    if let Some(input) = block.get("input") {
                        collect_file_paths(input, &mut files);
                    }
                }
                // Skip "thinking" and anything else.
                _ => {}
            }
        }
    } else if let Value::String(s) = content {
        // Some assistant messages may have a plain string content.
        text_parts.push(s.clone());
    }

    (text_parts.join("\n\n"), files)
}

/// Look for file-path keys inside a tool_use input object and collect their
/// values.
fn collect_file_paths(input: &Value, files: &mut Vec<String>) {
    const PATH_KEYS: &[&str] = &["file_path", "path", "filePath"];

    if let Value::Object(map) = input {
        for key in PATH_KEYS {
            if let Some(Value::String(p)) = map.get(*key) {
                let trimmed = p.trim();
                if !trimmed.is_empty() {
                    files.push(trimmed.to_string());
                }
            }
        }
    }
}

/// Decode a project directory name back to a real path.
///
/// Claude Code encodes project paths by replacing `/` with `-`, so
/// `-Users-anish-git-foo` becomes `/Users/anish/git/foo`.
fn decode_project_dir(encoded: &str) -> String {
    match encoded.strip_prefix('-') {
        Some(rest) => format!("/{}", rest.replace('-', "/")),
        None => encoded.replace('-', "/"),
    }
}

/// Truncate a string to at most `max` characters (on a char boundary).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_project_dir_basic() {
        assert_eq!(
            decode_project_dir("-Users-anish-git-foo"),
            "/Users/anish/git/foo"
        );
    }

    #[test]
    fn decode_project_dir_no_leading_dash() {
        assert_eq!(decode_project_dir("some-project"), "some/project");
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long() {
        let result = truncate("hello world this is a long string", 11);
        assert_eq!(result, "hello world...");
    }

    #[test]
    fn extract_user_content_string() {
        let v: Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": "hello world"
            }
        });
        assert_eq!(extract_user_content(&v), "hello world");
    }

    #[test]
    fn extract_user_content_array() {
        let v: Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {"type": "text", "text": "part one"},
                    {"type": "text", "text": "part two"}
                ]
            }
        });
        assert_eq!(extract_user_content(&v), "part one\n\npart two");
    }

    #[test]
    fn extract_assistant_text_only() {
        let v: Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "I will help."},
                    {"type": "thinking", "thinking": "let me think..."},
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "/foo/bar.rs"}}
                ]
            }
        });
        let (text, files) = extract_assistant_content(&v);
        assert_eq!(text, "I will help.");
        assert_eq!(files, vec!["/foo/bar.rs".to_string()]);
    }

    #[test]
    fn collect_file_paths_multiple_keys() {
        let input: Value = serde_json::json!({
            "file_path": "/a/b.rs",
            "path": "/c/d.rs",
            "filePath": "/e/f.rs",
            "command": "ls"
        });
        let mut files = Vec::new();
        collect_file_paths(&input, &mut files);
        assert_eq!(files.len(), 3);
        assert!(files.contains(&"/a/b.rs".to_string()));
        assert!(files.contains(&"/c/d.rs".to_string()));
        assert!(files.contains(&"/e/f.rs".to_string()));
    }
}
