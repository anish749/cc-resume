use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, AppMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    None,
    SearchChanged,
    ResumeSelected,
    Quit,
}

pub fn handle_key_event(app: &mut App, key: KeyEvent) -> InputAction {
    // Ctrl-C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return InputAction::Quit;
    }

    match app.mode {
        AppMode::Search => handle_search_mode(app, key),
        AppMode::Results => handle_results_mode(app, key),
    }
}

fn handle_search_mode(app: &mut App, key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Esc => InputAction::Quit,

        KeyCode::Tab => {
            if !app.results.is_empty() {
                app.mode = AppMode::Results;
            }
            InputAction::None
        }

        KeyCode::Enter => {
            if !app.results.is_empty() {
                app.mode = AppMode::Results;
            }
            InputAction::None
        }

        KeyCode::Up => {
            if !app.results.is_empty() {
                app.mode = AppMode::Results;
                app.select_previous();
            }
            InputAction::None
        }

        KeyCode::Down => {
            if !app.results.is_empty() {
                app.mode = AppMode::Results;
                app.select_next();
            }
            InputAction::None
        }

        KeyCode::Backspace => {
            if app.cursor_position > 0 {
                let byte_pos = app
                    .search_input
                    .char_indices()
                    .nth(app.cursor_position - 1)
                    .map(|(i, c)| (i, c.len_utf8()));
                if let Some((idx, len)) = byte_pos {
                    app.search_input.replace_range(idx..idx + len, "");
                    app.cursor_position -= 1;
                }
                return InputAction::SearchChanged;
            }
            InputAction::None
        }

        KeyCode::Left => {
            app.cursor_position = app.cursor_position.saturating_sub(1);
            InputAction::None
        }

        KeyCode::Right => {
            let char_count = app.search_input.chars().count();
            if app.cursor_position < char_count {
                app.cursor_position += 1;
            }
            InputAction::None
        }

        KeyCode::Home => {
            app.cursor_position = 0;
            InputAction::None
        }

        KeyCode::End => {
            app.cursor_position = app.search_input.chars().count();
            InputAction::None
        }

        KeyCode::Char(c) => {
            // Insert character at cursor position
            let byte_idx = app
                .search_input
                .char_indices()
                .nth(app.cursor_position)
                .map(|(i, _)| i)
                .unwrap_or(app.search_input.len());
            app.search_input.insert(byte_idx, c);
            app.cursor_position += 1;
            InputAction::SearchChanged
        }

        _ => InputAction::None,
    }
}

fn handle_results_mode(app: &mut App, key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Esc => {
            // Go back to search mode
            app.mode = AppMode::Search;
            InputAction::None
        }

        KeyCode::Tab | KeyCode::Char('/') => {
            app.mode = AppMode::Search;
            InputAction::None
        }

        KeyCode::Enter => InputAction::ResumeSelected,

        KeyCode::Up | KeyCode::Char('k') => {
            app.select_previous();
            InputAction::None
        }

        KeyCode::Down | KeyCode::Char('j') => {
            app.select_next();
            InputAction::None
        }

        KeyCode::Left | KeyCode::Char('h') => {
            app.scroll_preview_up();
            InputAction::None
        }

        KeyCode::Right | KeyCode::Char('l') => {
            app.scroll_preview_down();
            InputAction::None
        }

        KeyCode::Home => {
            if !app.results.is_empty() {
                app.selected_index = 0;
                app.load_preview();
            }
            InputAction::None
        }

        KeyCode::End => {
            if !app.results.is_empty() {
                app.selected_index = app.results.len() - 1;
                app.load_preview();
            }
            InputAction::None
        }

        KeyCode::Char(c) => {
            // Printable char: switch to search mode and append
            app.mode = AppMode::Search;
            let byte_idx = app
                .search_input
                .char_indices()
                .nth(app.cursor_position)
                .map(|(i, _)| i)
                .unwrap_or(app.search_input.len());
            app.search_input.insert(byte_idx, c);
            app.cursor_position += 1;
            InputAction::SearchChanged
        }

        _ => InputAction::None,
    }
}
