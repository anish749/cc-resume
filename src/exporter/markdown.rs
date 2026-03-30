use super::parser::{MessageRole, SessionMessage, SessionMetadata};

/// Maximum number of characters for an assistant message block in the output.
const MAX_ASSISTANT_CHARS: usize = 2000;

// ---------------------------------------------------------------------------
// Session document model
// ---------------------------------------------------------------------------

/// The typed frontmatter of a session markdown document.
/// New fields added here are automatically handled by parse/render.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Frontmatter {
    pub session_id: String,
    pub project_name: String,
    pub project_path: String,
    pub date: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub files_touched: Vec<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    // AI-generated summary fields
    pub ai_summary: Option<String>,
    pub ai_topics: Option<Vec<String>>,
    pub ai_intent: Option<String>,
}

/// A complete session markdown document: typed frontmatter + raw body.
#[derive(Debug, Clone)]
pub(crate) struct SessionDocument {
    pub frontmatter: Frontmatter,
    /// Everything after the closing `---` of the frontmatter (including the heading and messages).
    pub body: String,
}

impl SessionDocument {
    /// Parse a session markdown string into a typed document.
    pub fn parse(content: &str) -> Option<Self> {
        if !content.starts_with("---\n") {
            return None;
        }

        let rest = &content[4..]; // skip opening "---\n"
        let closing_pos = rest.find("\n---\n")?;
        let frontmatter_str = &rest[..closing_pos];
        let body = rest[closing_pos + 5..].to_string(); // skip "\n---\n"

        let frontmatter = Frontmatter::parse(frontmatter_str);
        Some(Self { frontmatter, body })
    }

    /// Render the document back to a markdown string.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.body.len() + 2048);
        out.push_str("---\n");
        self.frontmatter.render(&mut out);
        out.push_str("---\n");
        out.push_str(&self.body);
        out
    }
}

impl Frontmatter {
    /// Parse frontmatter fields from the YAML text between the `---` delimiters.
    fn parse(yaml_text: &str) -> Self {
        let mut fm = Frontmatter::default();
        let mut current_list: Option<&str> = None; // tracks which list field we're collecting

        for line in yaml_text.lines() {
            // List continuation line
            if line.starts_with("  - ") {
                let value = line[4..].trim().to_string();
                let value = unquote(&value);
                match current_list {
                    Some("files_touched") => fm.files_touched.push(value),
                    Some("ai_topics") => {
                        fm.ai_topics.get_or_insert_with(Vec::new).push(value);
                    }
                    _ => {}
                }
                continue;
            }

            // Not a continuation — reset list context
            current_list = None;

            // Key: value line
            if let Some((key, value)) = split_yaml_line(line) {
                let value = unquote(value);
                match key {
                    "session_id" => fm.session_id = value,
                    "project_name" => fm.project_name = value,
                    "project_path" => fm.project_path = value,
                    "date" => fm.date = non_null(value),
                    "git_branch" => fm.git_branch = non_null(value),
                    "first_prompt" => fm.first_prompt = non_null(value),
                    "started_at" => fm.started_at = non_null(value),
                    "ended_at" => fm.ended_at = non_null(value),
                    "ai_summary" => fm.ai_summary = non_null(value),
                    "ai_intent" => fm.ai_intent = non_null(value),
                    "files_touched" => {
                        if value == "[]" {
                            fm.files_touched = Vec::new();
                        } else {
                            // Empty value or anything else means list items follow
                            current_list = Some("files_touched");
                        }
                    }
                    "ai_topics" => {
                        if value == "[]" {
                            fm.ai_topics = Some(Vec::new());
                        } else {
                            current_list = Some("ai_topics");
                        }
                    }
                    _ => {} // ignore unknown fields
                }
            }
        }

        fm
    }

    /// Render frontmatter fields as YAML text (without the `---` delimiters).
    fn render(&self, out: &mut String) {
        write_yaml_field(out, "session_id", &self.session_id);
        write_yaml_field(out, "project_name", &self.project_name);
        write_yaml_field(out, "project_path", &self.project_path);
        write_yaml_opt(out, "date", self.date.as_deref());
        write_yaml_opt(out, "git_branch", self.git_branch.as_deref());
        write_yaml_opt(out, "first_prompt", self.first_prompt.as_deref());

        write_yaml_list(out, "files_touched", &self.files_touched);

        write_yaml_opt(out, "started_at", self.started_at.as_deref());
        write_yaml_opt(out, "ended_at", self.ended_at.as_deref());

        // AI summary fields — only written when present
        if let Some(ref summary) = self.ai_summary {
            write_yaml_field(out, "ai_summary", summary);
        }
        if let Some(ref topics) = self.ai_topics {
            write_yaml_list(out, "ai_topics", topics);
        }
        if let Some(ref intent) = self.ai_intent {
            write_yaml_field(out, "ai_intent", intent);
        }
    }
}

/// Build a `SessionDocument` from parsed session data (initial export).
pub fn render(metadata: &SessionMetadata, messages: &[SessionMessage]) -> String {
    let frontmatter = Frontmatter {
        session_id: metadata.session_id.clone(),
        project_name: metadata.project_name.clone(),
        project_path: metadata.project_path.clone(),
        date: metadata.date.clone(),
        git_branch: metadata.git_branch.clone(),
        first_prompt: metadata.first_prompt.clone(),
        files_touched: metadata.files_touched.clone(),
        started_at: metadata.started_at.clone(),
        ended_at: metadata.ended_at.clone(),
        ai_summary: None,
        ai_topics: None,
        ai_intent: None,
    };

    let mut body = String::with_capacity(8 * 1024);

    // Heading
    let short_id: String = metadata.session_id.chars().take(8).collect();
    let date_str = metadata.date.as_deref().unwrap_or("unknown");
    body.push_str(&format!("\n# Session: {date_str} ({short_id})\n\n"));

    // Messages
    for msg in messages {
        match msg.role {
            MessageRole::User => {
                body.push_str("## User\n\n");
                body.push_str(&msg.content);
                body.push_str("\n\n");
            }
            MessageRole::Assistant => {
                body.push_str("## Assistant\n\n");
                let text = truncate_content(&msg.content, MAX_ASSISTANT_CHARS);
                body.push_str(&text);
                body.push_str("\n\n");
            }
        }
    }

    let doc = SessionDocument { frontmatter, body };
    doc.render()
}

/// Inject AI summary fields into an existing session markdown file.
/// Parses the document, sets the summary fields, and writes it back.
pub(crate) fn inject_summary(
    md_path: &std::path::Path,
    summary: &str,
    topics: &[String],
    intent: &str,
) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(md_path)?;
    let mut doc = SessionDocument::parse(&content)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse session document"))?;

    doc.frontmatter.ai_summary = Some(summary.to_string());
    doc.frontmatter.ai_topics = Some(topics.to_vec());
    doc.frontmatter.ai_intent = Some(intent.to_string());

    std::fs::write(md_path, doc.render())?;
    Ok(())
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

/// Write an optional YAML scalar field. Writes `null` when `None`.
fn write_yaml_opt(out: &mut String, key: &str, value: Option<&str>) {
    match value {
        Some(v) => write_yaml_field(out, key, v),
        None => {
            out.push_str(key);
            out.push_str(": null\n");
        }
    }
}

/// Write a YAML list field.
fn write_yaml_list(out: &mut String, key: &str, items: &[String]) {
    if items.is_empty() {
        out.push_str(key);
        out.push_str(": []\n");
    } else {
        out.push_str(key);
        out.push_str(":\n");
        for item in items {
            out.push_str("  - ");
            out.push_str(&yaml_escape_value(item));
            out.push('\n');
        }
    }
}

/// Escape a YAML scalar value if it contains characters that could confuse a
/// YAML parser. We always quote when in doubt.
pub(crate) fn yaml_escape_value(value: &str) -> String {
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

/// Split a YAML line into (key, value). Handles `key: value` and `key:` (empty value).
fn split_yaml_line(line: &str) -> Option<(&str, &str)> {
    let colon_pos = line.find(':')?;
    let key = line[..colon_pos].trim();
    if key.is_empty() || key.starts_with(' ') || key.starts_with('-') {
        return None; // not a top-level key
    }
    let value = line[colon_pos + 1..].trim();
    Some((key, value))
}

/// Remove surrounding quotes from a YAML value and unescape basic sequences.
fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        let inner = &trimmed[1..trimmed.len() - 1];
        inner
            .replace("\\n", "\n")
            .replace("\\r", "\r")
            .replace("\\t", "\t")
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        trimmed.to_string()
    }
}

/// Convert a YAML value to `Option<String>`, treating "null" as `None`.
fn non_null(value: String) -> Option<String> {
    if value == "null" || value.is_empty() {
        None
    } else {
        Some(value)
    }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    fn sample_md() -> String {
        render(&sample_metadata(), &sample_messages())
    }

    // --- Render tests ---

    #[test]
    fn render_produces_frontmatter_and_body() {
        let md = sample_md();

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
    fn render_without_ai_fields_does_not_include_them() {
        let md = sample_md();
        assert!(!md.contains("ai_summary"));
        assert!(!md.contains("ai_topics"));
        assert!(!md.contains("ai_intent"));
    }

    // --- Parse/roundtrip tests ---

    #[test]
    fn parse_roundtrip_preserves_content() {
        let md = sample_md();
        let doc = SessionDocument::parse(&md).expect("should parse");

        assert_eq!(doc.frontmatter.session_id, "abcdef12-3456-7890-abcd-ef1234567890");
        assert_eq!(doc.frontmatter.project_path, "/Users/anish/git/myproj");
        assert_eq!(doc.frontmatter.date, Some("2025-04-15".to_string()));
        assert_eq!(doc.frontmatter.git_branch, Some("main".to_string()));
        assert_eq!(doc.frontmatter.files_touched, vec!["src/auth.rs", "src/main.rs"]);
        assert!(doc.frontmatter.ai_summary.is_none());
        assert!(doc.frontmatter.ai_topics.is_none());
        assert!(doc.frontmatter.ai_intent.is_none());

        // Re-render and re-parse should be stable
        let rendered = doc.render();
        let doc2 = SessionDocument::parse(&rendered).expect("should re-parse");
        assert_eq!(doc.frontmatter, doc2.frontmatter);
    }

    // --- Summary injection tests ---

    #[test]
    fn inject_summary_adds_ai_fields() {
        let md = sample_md();
        let mut doc = SessionDocument::parse(&md).unwrap();

        doc.frontmatter.ai_summary = Some("Fixed a login bug in auth.rs".to_string());
        doc.frontmatter.ai_topics = Some(vec![
            "Debugged authentication failure in the login flow".to_string(),
            "Fixed token validation logic in auth middleware".to_string(),
        ]);
        doc.frontmatter.ai_intent = Some("bug-fix".to_string());

        let rendered = doc.render();

        assert!(rendered.contains("ai_summary: "));
        assert!(rendered.contains("ai_topics:"));
        assert!(rendered.contains("  - "));
        assert!(rendered.contains("ai_intent: "));
        // Body preserved
        assert!(rendered.contains("## User\n\nFix the login bug"));
    }

    #[test]
    fn inject_summary_roundtrip() {
        let md = sample_md();
        let mut doc = SessionDocument::parse(&md).unwrap();

        let summary = "Fixed a login bug in auth.rs by correcting token validation.";
        let topics = vec![
            "Debugged authentication failure caused by expired token check".to_string(),
            "Updated the auth middleware to handle edge cases with refresh tokens".to_string(),
        ];
        let intent = "bug-fix";

        doc.frontmatter.ai_summary = Some(summary.to_string());
        doc.frontmatter.ai_topics = Some(topics.clone());
        doc.frontmatter.ai_intent = Some(intent.to_string());

        let rendered = doc.render();
        let reparsed = SessionDocument::parse(&rendered).unwrap();

        assert_eq!(reparsed.frontmatter.ai_summary.as_deref(), Some(summary));
        assert_eq!(reparsed.frontmatter.ai_topics.as_ref(), Some(&topics));
        assert_eq!(reparsed.frontmatter.ai_intent.as_deref(), Some(intent));
        // Original fields preserved
        assert_eq!(reparsed.frontmatter.session_id, doc.frontmatter.session_id);
        assert_eq!(reparsed.frontmatter.files_touched, doc.frontmatter.files_touched);
    }

    #[test]
    fn inject_summary_overwrites_existing_ai_fields() {
        let md = sample_md();
        let mut doc = SessionDocument::parse(&md).unwrap();

        // First injection
        doc.frontmatter.ai_summary = Some("Old summary".to_string());
        doc.frontmatter.ai_topics = Some(vec!["Old topic".to_string()]);
        doc.frontmatter.ai_intent = Some("exploration".to_string());

        let rendered1 = doc.render();

        // Second injection (simulates re-summarization)
        let mut doc2 = SessionDocument::parse(&rendered1).unwrap();
        doc2.frontmatter.ai_summary = Some("New summary".to_string());
        doc2.frontmatter.ai_topics = Some(vec![
            "New topic one".to_string(),
            "New topic two".to_string(),
        ]);
        doc2.frontmatter.ai_intent = Some("bug-fix".to_string());

        let rendered2 = doc2.render();
        let final_doc = SessionDocument::parse(&rendered2).unwrap();

        assert_eq!(final_doc.frontmatter.ai_summary.as_deref(), Some("New summary"));
        assert_eq!(
            final_doc.frontmatter.ai_topics.as_ref().unwrap(),
            &vec!["New topic one".to_string(), "New topic two".to_string()]
        );
        assert_eq!(final_doc.frontmatter.ai_intent.as_deref(), Some("bug-fix"));
        // No trace of old values
        assert!(!rendered2.contains("Old summary"));
        assert!(!rendered2.contains("Old topic"));
        assert!(!rendered2.contains("exploration"));
    }

    #[test]
    fn parse_returns_none_for_no_frontmatter() {
        assert!(SessionDocument::parse("# Just a heading\n\nSome content").is_none());
    }

    #[test]
    fn empty_files_touched_roundtrips() {
        let md = render(
            &SessionMetadata {
                files_touched: vec![],
                ..sample_metadata()
            },
            &sample_messages(),
        );
        let doc = SessionDocument::parse(&md).unwrap();
        assert!(doc.frontmatter.files_touched.is_empty());
    }

    #[test]
    fn empty_topics_roundtrips() {
        let md = sample_md();
        let mut doc = SessionDocument::parse(&md).unwrap();
        doc.frontmatter.ai_topics = Some(vec![]);
        doc.frontmatter.ai_summary = Some("test".to_string());
        doc.frontmatter.ai_intent = Some("feature".to_string());

        let rendered = doc.render();
        let reparsed = SessionDocument::parse(&rendered).unwrap();
        assert_eq!(reparsed.frontmatter.ai_topics, Some(vec![]));
    }

    // --- YAML escape tests ---

    #[test]
    fn yaml_escape_plain() {
        assert_eq!(yaml_escape_value("hello"), "hello");
    }

    #[test]
    fn yaml_escape_colon() {
        assert_eq!(yaml_escape_value("key: value"), r#""key: value""#);
    }

    #[test]
    fn yaml_escape_newline() {
        assert_eq!(yaml_escape_value("line1\nline2"), r#""line1\nline2""#);
    }

    #[test]
    fn yaml_escape_bool() {
        assert_eq!(yaml_escape_value("true"), r#""true""#);
        assert_eq!(yaml_escape_value("false"), r#""false""#);
    }

    #[test]
    fn yaml_escape_quotes_in_value() {
        let escaped = yaml_escape_value(r#"She said "hello""#);
        assert!(escaped.starts_with('"'));
        assert!(escaped.contains(r#"\"hello\""#));
    }

    // --- Helper tests ---

    #[test]
    fn unquote_double_quoted() {
        assert_eq!(unquote(r#""hello world""#), "hello world");
    }

    #[test]
    fn unquote_with_escapes() {
        assert_eq!(unquote(r#""line1\nline2""#), "line1\nline2");
    }

    #[test]
    fn unquote_plain() {
        assert_eq!(unquote("plain value"), "plain value");
    }

    #[test]
    fn split_yaml_line_basic() {
        let (key, val) = split_yaml_line("date: 2025-04-15").unwrap();
        assert_eq!(key, "date");
        assert_eq!(val, "2025-04-15");
    }

    #[test]
    fn split_yaml_line_empty_value() {
        let (key, val) = split_yaml_line("files_touched:").unwrap();
        assert_eq!(key, "files_touched");
        assert_eq!(val, "");
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
        assert!(result.starts_with(&"a".repeat(2000)));
    }
}
