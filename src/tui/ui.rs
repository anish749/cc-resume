use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};

use super::app::{App, AppMode};

/// Initialize the terminal for TUI rendering.
pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its original state.
pub fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    Ok(())
}

/// Draw the entire TUI layout.
pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    // Main layout: search bar (3) | content (rest) | status bar (1)
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // search bar
            Constraint::Min(5),    // content area
            Constraint::Length(1), // status bar
        ])
        .split(size);

    draw_search_bar(f, app, main_chunks[0]);
    draw_content(f, app, main_chunks[1]);
    draw_status_bar(f, app, main_chunks[2]);
}

/// Draw the search bar at the top.
fn draw_search_bar(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.mode == AppMode::Search;

    let border_style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let search_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" Search ");

    // Build the search text with cursor
    let display_text = if is_active {
        // Show cursor as a block character
        let input = &app.search_input;
        let chars: Vec<char> = input.chars().collect();
        let (before, cursor_char, after) = if app.cursor_position < chars.len() {
            let before: String = chars[..app.cursor_position].iter().collect();
            let cursor: String = chars[app.cursor_position..=app.cursor_position].iter().collect();
            let after: String = chars[app.cursor_position + 1..].iter().collect();
            (before, cursor, after)
        } else {
            (input.clone(), " ".to_string(), String::new())
        };

        Line::from(vec![
            Span::styled("Search: ", Style::default().fg(Color::Yellow)),
            Span::raw(before),
            Span::styled(cursor_char, Style::default().bg(Color::White).fg(Color::Black)),
            Span::raw(after),
        ])
    } else {
        Line::from(vec![
            Span::styled("Search: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&app.search_input),
        ])
    };

    let search_paragraph = Paragraph::new(display_text).block(search_block);
    f.render_widget(search_paragraph, area);
}

/// Draw the main content area: results list + preview pane.
fn draw_content(f: &mut Frame, app: &App, area: Rect) {
    // Horizontal split: results (50%) | preview (50%)
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_results(f, app, content_chunks[0]);
    draw_preview(f, app, content_chunks[1]);
}

/// Draw the results list.
fn draw_results(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.mode == AppMode::Results;

    let border_style = if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let results_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(format!(" Results ({}) ", app.results.len()));

    if app.results.is_empty() {
        let empty_msg = if app.search_input.is_empty() {
            "Type to search sessions..."
        } else {
            "No results found."
        };
        let paragraph = Paragraph::new(empty_msg)
            .block(results_block)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(paragraph, area);
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let is_selected = i == app.selected_index;

            // Score as percentage
            let score_str = format!("{:3.0}%", result.score * 100.0);

            // Date, shortened
            let date_str = result
                .date
                .as_deref()
                .unwrap_or("???")
                .chars()
                .take(10)
                .collect::<String>();

            // Project name: just the last path component
            let project_str = result
                .project_name
                .as_deref()
                .or(result.project_path.as_deref())
                .map(|p| {
                    p.rsplit('/')
                        .next()
                        .unwrap_or(p)
                        .to_string()
                })
                .unwrap_or_default();

            // Git branch
            let branch_str = result
                .git_branch
                .as_deref()
                .map(|b| format!(" [{}]", truncate_str(b, 15)))
                .unwrap_or_default();

            // First line: score, date, project, branch
            // Second line: first prompt, truncated
            let prompt_str = result
                .first_prompt
                .as_deref()
                .map(|p| {
                    let sanitized = p.replace('\n', " ");
                    let max_width = area.width.saturating_sub(6) as usize;
                    truncate_str(&sanitized, max_width).to_string()
                })
                .unwrap_or_default();

            let style = if is_selected {
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let marker = if is_selected { "> " } else { "  " };

            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        marker,
                        if is_selected {
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(
                        score_str,
                        Style::default().fg(Color::Green).add_modifier(if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                    ),
                    Span::raw("  "),
                    Span::styled(date_str, Style::default().fg(Color::Yellow)),
                    Span::raw("  "),
                    Span::styled(project_str, Style::default().fg(Color::Blue)),
                    Span::styled(branch_str, Style::default().fg(Color::Magenta)),
                ]),
                Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        prompt_str,
                        Style::default().fg(if is_selected {
                            Color::White
                        } else {
                            Color::Gray
                        }),
                    ),
                ]),
            ];

            ListItem::new(lines).style(style)
        })
        .collect();

    // Calculate visible window for scrolling
    let visible_height = area.height.saturating_sub(2) as usize; // subtract borders
    let items_per_result = 2; // each result takes 2 lines
    let visible_items = visible_height / items_per_result;

    // Determine the scroll offset to keep selected item visible
    let scroll_offset = if visible_items == 0 {
        0
    } else if app.selected_index >= visible_items {
        app.selected_index - visible_items + 1
    } else {
        0
    };

    let visible_items_list: Vec<ListItem> = items
        .into_iter()
        .skip(scroll_offset)
        .collect();

    let list = List::new(visible_items_list).block(results_block);
    f.render_widget(list, area);
}

/// Draw the preview pane.
fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let preview_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Preview ");

    match &app.preview_content {
        Some(content) => {
            // Simple markdown-ish rendering: colorize headers and code blocks
            let lines = render_preview_lines(content, area.width.saturating_sub(2) as usize);
            let paragraph = Paragraph::new(lines)
                .block(preview_block)
                .wrap(Wrap { trim: false })
                .scroll((app.preview_scroll, 0));
            f.render_widget(paragraph, area);
        }
        None => {
            let msg = if app.results.is_empty() {
                "Search for sessions to see a preview."
            } else {
                "No preview available."
            };
            let paragraph = Paragraph::new(msg)
                .block(preview_block)
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(paragraph, area);
        }
    }
}

/// Render preview content with basic markdown styling.
fn render_preview_lines(content: &str, _max_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;

    for line in content.lines() {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        } else if in_code_block {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::Green),
            )));
        } else if line.starts_with("# ") {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if line.starts_with("## ") {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if line.starts_with("### ") {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if line.starts_with("---") {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        } else if line.starts_with("> ") {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            lines.push(Line::from(line.to_string()));
        }
    }

    lines
}

/// Draw the status bar at the bottom.
fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let status_line = if let Some(ref msg) = app.status_message {
        Line::from(vec![Span::styled(
            msg.clone(),
            Style::default().fg(Color::Red),
        )])
    } else {
        let mode_indicator = match app.mode {
            AppMode::Search => Span::styled(
                " SEARCH ",
                Style::default().bg(Color::Cyan).fg(Color::Black),
            ),
            AppMode::Results => Span::styled(
                " RESULTS ",
                Style::default().bg(Color::Green).fg(Color::Black),
            ),
        };

        let search_status = if app.searching {
            Span::styled(" searching... ", Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC))
        } else if let Some(elapsed) = app.last_search_time {
            let secs = elapsed.as_secs_f64();
            Span::styled(
                format!(" {} results in {secs:.1}s ", app.result_count),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        };

        Line::from(vec![
            mode_indicator,
            search_status,
            Span::raw(" "),
            Span::styled("[Tab]", Style::default().fg(Color::Yellow)),
            Span::raw(" switch  "),
            Span::styled("[Enter]", Style::default().fg(Color::Yellow)),
            Span::raw(" resume  "),
            Span::styled("[Esc]", Style::default().fg(Color::Yellow)),
            Span::raw(" quit  "),
            Span::styled("[Up/Down]", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("[PgUp/PgDn]", Style::default().fg(Color::Yellow)),
            Span::raw(" scroll"),
        ])
    };

    let status_bar = Paragraph::new(status_line)
        .style(Style::default().bg(Color::Black));
    f.render_widget(status_bar, area);
}

/// Truncate a string to at most `max_chars` characters, adding "..." if truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars <= 3 {
        return s.chars().take(max_chars).collect();
    }
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 3).collect();
        format!("{truncated}...")
    }
}
