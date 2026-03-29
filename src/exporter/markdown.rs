use super::parser::{MessageRole, SessionMessage, SessionMetadata};

/// Maximum number of characters for an assistant message block in the output.
const MAX_ASSISTANT_CHARS: usize = 2000;

/// Render a session as a Markdown document with YAML frontmatter.
pub fn render(metadata: &SessionMetadata, messages: &[SessionMessage]) -> String {
    let mut out = String::with_capacity(8 * 1024);

    // --- YAML frontmatter ---
    out.push_str("---\n");
    write_yaml_field(&mut out, "session_id", &metadata.session_id);
    write_yaml_field(&mut out, "project_name", &metadata.project_name);
    write_yaml_field(&mut out, "project_path", &metadata.project_path);
    write_yaml_opt(&mut out, "date", metadata.date.as_deref());
    write_yaml_opt(&mut out, "git_branch", metadata.git_branch.as_deref());
    write_yaml_opt(&mut out, "first_prompt", metadata.first_prompt.as_deref());

    if metadata.files_touched.is_empty() {
        out.push_str("files_touched: []\n");
    } else {
        out.push_str("files_touched:\n");
        for f in &metadata.files_touched {
            out.push_str("  - ");
            out.push_str(&yaml_escape_value(f));
            out.push('\n');
        }
    }

    write_yaml_opt(&mut out, "started_at", metadata.started_at.as_deref());
    write_yaml_opt(&mut out, "ended_at", metadata.ended_at.as_deref());
    out.push_str("---\n\n");

    // --- Heading ---
    let short_id: String = metadata.session_id.chars().take(8).collect();
    let date_str = metadata.date.as_deref().unwrap_or("unknown");
    out.push_str(&format!("# Session: {date_str} ({short_id})\n\n"));

    // --- Messages ---
    for msg in messages {
        match msg.role {
            MessageRole::User => {
                out.push_str("## User\n\n");
                out.push_str(&msg.content);
                out.push_str("\n\n");
            }
            MessageRole::Assistant => {
                out.push_str("## Assistant\n\n");
                let text = truncate_content(&msg.content, MAX_ASSISTANT_CHARS);
                out.push_str(&text);
                out.push_str("\n\n");
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// YAML helpers
// ---------------------------------------------------------------------------

/// Write a required YAML scalar field.
fn write_yaml_field(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&yaml_escape_value(value));
    out.push('\n');
}

/// Write an optional YAML scalar field. Omits the field when `None`.
fn write_yaml_opt(out: &mut String, key: &str, value: Option<&str>) {
    match value {
        Some(v) => write_yaml_field(out, key, v),
        None => {
            out.push_str(key);
            out.push_str(": null\n");
        }
    }
}

/// Escape a YAML scalar value if it contains characters that could confuse a
/// YAML parser. We always quote when in doubt.
fn yaml_escape_value(value: &str) -> String {
    // Characters that signal "quote this value".
    let needs_quoting = value.is_empty()
        || value.contains(':')
        || value.contains('#')
        || value.contains('\'')
        || value.contains('"')
        || value.contains('\n')
        || value.contains('\r')
        || value.contains('\\')
        || value.contains('{')
        || value.contains('}')
        || value.contains('[')
        || value.contains(']')
        || value.contains(',')
        || value.contains('&')
        || value.contains('*')
        || value.contains('?')
        || value.contains('|')
        || value.contains('>')
        || value.contains('!')
        || value.contains('%')
        || value.contains('@')
        || value.contains('`')
        || value.starts_with('-')
        || value.starts_with(' ')
        || value.ends_with(' ')
        || looks_like_yaml_special(value);

    if !needs_quoting {
        return value.to_string();
    }

    // Use double-quoted form with escaped internals.
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");

    format!("\"{escaped}\"")
}

/// Returns true for values that YAML might interpret as booleans, nulls, or
/// numbers (e.g. "true", "null", "1.5").
fn looks_like_yaml_special(value: &str) -> bool {
    let lower = value.to_lowercase();
    matches!(
        lower.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
    ) || value.parse::<f64>().is_ok()
}

/// Truncate content to `max` characters, appending an ellipsis indicator if
/// truncated.
fn truncate_content(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}\n\n[...truncated]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exporter::parser::{MessageRole, SessionMessage, SessionMetadata};

    fn sample_metadata() -> SessionMetadata {
        SessionMetadata {
            session_id: "abcdef12-3456-7890-abcd-ef1234567890".to_string(),
            project_name: "-Users-anish-git-myproj".to_string(),
            project_path: "/Users/anish/git/myproj".to_string(),
            date: Some("2025-04-15".to_string()),
            git_branch: Some("main".to_string()),
            first_prompt: Some("Fix the login bug".to_string()),
            files_touched: vec![
                "src/auth.rs".to_string(),
                "src/main.rs".to_string(),
            ],
            started_at: Some("2025-04-15T10:00:00Z".to_string()),
            ended_at: Some("2025-04-15T10:30:00Z".to_string()),
        }
    }

    fn sample_messages() -> Vec<SessionMessage> {
        vec![
            SessionMessage {
                role: MessageRole::User,
                content: "Fix the login bug".to_string(),
                timestamp: Some("2025-04-15T10:00:00Z".to_string()),
            },
            SessionMessage {
                role: MessageRole::Assistant,
                content: "I found the issue in auth.rs.".to_string(),
                timestamp: Some("2025-04-15T10:01:00Z".to_string()),
            },
        ]
    }

    #[test]
    fn render_produces_frontmatter_and_body() {
        let md = render(&sample_metadata(), &sample_messages());

        assert!(md.starts_with("---\n"));
        assert!(md.contains("session_id: abcdef12-3456-7890-abcd-ef1234567890\n"));
        assert!(md.contains("date: 2025-04-15\n"));
        assert!(md.contains("git_branch: main\n"));
        assert!(md.contains("files_touched:\n  - src/auth.rs\n  - src/main.rs\n"));
        assert!(md.contains("# Session: 2025-04-15 (abcdef12)"));
        assert!(md.contains("## User\n\nFix the login bug"));
        assert!(md.contains("## Assistant\n\nI found the issue in auth.rs."));
    }

    #[test]
    fn yaml_escape_plain() {
        assert_eq!(yaml_escape_value("hello"), "hello");
    }

    #[test]
    fn yaml_escape_colon() {
        let escaped = yaml_escape_value("key: value");
        assert_eq!(escaped, r#""key: value""#);
    }

    #[test]
    fn yaml_escape_newline() {
        let escaped = yaml_escape_value("line1\nline2");
        assert_eq!(escaped, r#""line1\nline2""#);
    }

    #[test]
    fn yaml_escape_bool() {
        assert_eq!(yaml_escape_value("true"), r#""true""#);
        assert_eq!(yaml_escape_value("false"), r#""false""#);
    }

    #[test]
    fn truncate_content_short() {
        assert_eq!(truncate_content("short", 100), "short");
    }

    #[test]
    fn truncate_content_long() {
        let long = "a".repeat(3000);
        let result = truncate_content(&long, 2000);
        assert!(result.ends_with("\n\n[...truncated]"));
        // 2000 'a' chars + "\n\n[...truncated]"
        assert!(result.starts_with(&"a".repeat(2000)));
    }
}
