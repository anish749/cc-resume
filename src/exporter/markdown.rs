use serde::{Deserialize, Serialize};

use super::parser::{MessageRole, SessionMessage, SessionMetadata};

/// Maximum number of characters for an assistant message block in the output.
const MAX_ASSISTANT_CHARS: usize = 2000;

// ---------------------------------------------------------------------------
// Session document model
// ---------------------------------------------------------------------------

/// The typed frontmatter of a session markdown document.
/// Uses serde_yaml for parsing and rendering — adding a new field is just
/// adding a struct field with the right serde attributes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Frontmatter {
    pub session_id: String,
    pub project_name: String,
    pub project_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    // AI-generated summary fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_topics: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

        let frontmatter: Frontmatter = serde_yaml::from_str(frontmatter_str).ok()?;
        Some(Self { frontmatter, body })
    }

    /// Render the document back to a markdown string.
    pub fn render(&self) -> String {
        let yaml = serde_yaml::to_string(&self.frontmatter)
            .expect("Frontmatter serialization should not fail");

        let mut out = String::with_capacity(yaml.len() + self.body.len() + 16);
        out.push_str("---\n");
        out.push_str(&yaml);
        out.push_str("---\n");
        out.push_str(&self.body);
        out
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
// Helpers
// ---------------------------------------------------------------------------

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

        assert!(rendered.contains("ai_summary:"));
        assert!(rendered.contains("ai_topics:"));
        assert!(rendered.contains("ai_intent:"));
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

    // --- Serde roundtrip edge cases ---

    #[test]
    fn special_characters_in_first_prompt_roundtrip() {
        let md = render(
            &SessionMetadata {
                first_prompt: Some("Fix the bug: it's \"broken\" & won't work\nnewline here".to_string()),
                ..sample_metadata()
            },
            &sample_messages(),
        );
        let doc = SessionDocument::parse(&md).unwrap();
        assert_eq!(
            doc.frontmatter.first_prompt.as_deref(),
            Some("Fix the bug: it's \"broken\" & won't work\nnewline here")
        );

        // Double roundtrip
        let rendered = doc.render();
        let doc2 = SessionDocument::parse(&rendered).unwrap();
        assert_eq!(doc.frontmatter.first_prompt, doc2.frontmatter.first_prompt);
    }

    #[test]
    fn special_characters_in_topics_roundtrip() {
        let md = sample_md();
        let mut doc = SessionDocument::parse(&md).unwrap();

        let topics = vec![
            "Debugged the auth: tokens weren't refreshing properly".to_string(),
            "Fixed \"edge case\" where user's session expired & caused a crash".to_string(),
            "Refactored code with special chars like {braces}, [brackets], *asterisks*".to_string(),
        ];
        doc.frontmatter.ai_topics = Some(topics.clone());
        doc.frontmatter.ai_summary = Some("Summary with: colons & \"quotes\"".to_string());
        doc.frontmatter.ai_intent = Some("bug-fix".to_string());

        let rendered = doc.render();
        let reparsed = SessionDocument::parse(&rendered).unwrap();

        assert_eq!(reparsed.frontmatter.ai_topics.as_ref(), Some(&topics));
        assert_eq!(
            reparsed.frontmatter.ai_summary.as_deref(),
            Some("Summary with: colons & \"quotes\"")
        );
    }

    #[test]
    fn project_path_with_dashes_roundtrip() {
        let md = render(
            &SessionMetadata {
                project_name: "-Users-anish-my-cool-project".to_string(),
                project_path: "/Users/anish/my-cool-project".to_string(),
                ..sample_metadata()
            },
            &sample_messages(),
        );
        let doc = SessionDocument::parse(&md).unwrap();
        assert_eq!(doc.frontmatter.project_path, "/Users/anish/my-cool-project");
        assert_eq!(doc.frontmatter.project_name, "-Users-anish-my-cool-project");
    }

    #[test]
    fn all_optional_fields_none_roundtrip() {
        let md = render(
            &SessionMetadata {
                session_id: "test-id".to_string(),
                project_name: "test".to_string(),
                project_path: "/test".to_string(),
                date: None,
                git_branch: None,
                first_prompt: None,
                files_touched: vec![],
                started_at: None,
                ended_at: None,
            },
            &sample_messages(),
        );
        let doc = SessionDocument::parse(&md).unwrap();
        assert!(doc.frontmatter.date.is_none());
        assert!(doc.frontmatter.git_branch.is_none());
        assert!(doc.frontmatter.first_prompt.is_none());
        assert!(doc.frontmatter.started_at.is_none());
        assert!(doc.frontmatter.ended_at.is_none());
        assert!(doc.frontmatter.ai_summary.is_none());
    }

    #[test]
    fn parse_tolerates_unknown_fields() {
        // Simulate a frontmatter with an extra field we don't know about
        let content = "---\nsession_id: abc\nproject_name: test\nproject_path: /test\nunknown_field: some_value\n---\nbody";
        let doc = SessionDocument::parse(content).unwrap();
        assert_eq!(doc.frontmatter.session_id, "abc");
        assert_eq!(doc.body, "body");
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
