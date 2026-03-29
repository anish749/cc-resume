use anyhow::Result;
use crossterm::event::{self, Event};
use ratatui::backend::Backend;
use ratatui::Terminal;
use std::time::Duration;
use tokio::time::Instant;

use crate::qmd::{QmdClient, SearchResult};

use super::input::{self, InputAction};
use super::ui;

pub struct App {
    pub qmd: QmdClient,
    pub search_input: String,
    pub cursor_position: usize,
    pub results: Vec<SearchResult>,
    pub selected_index: usize,
    pub preview_content: Option<String>,
    pub mode: AppMode,
    pub should_quit: bool,
    pub search_debounce: Option<Instant>,
    pub status_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Search,
    Results,
}

impl App {
    pub fn new(qmd: QmdClient) -> Self {
        Self {
            qmd,
            search_input: String::new(),
            cursor_position: 0,
            results: Vec::new(),
            selected_index: 0,
            preview_content: None,
            mode: AppMode::Search,
            should_quit: false,
            search_debounce: None,
            status_message: None,
        }
    }

    pub async fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        // Do an initial empty search to populate results with recent sessions
        self.trigger_search().await;

        loop {
            terminal.draw(|f| ui::draw(f, self))?;

            if self.should_quit {
                return Ok(());
            }

            // Check if a debounced search should fire
            if let Some(debounce_instant) = self.search_debounce {
                let now = Instant::now();
                if now.duration_since(debounce_instant) >= Duration::from_millis(300) {
                    self.search_debounce = None;
                    self.trigger_search().await;
                }
            }

            // Poll for crossterm events with a 100ms timeout
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    let action = input::handle_key_event(self, key);
                    match action {
                        InputAction::None => {}
                        InputAction::SearchChanged => {
                            // Record the debounce instant; search fires after 300ms of inactivity
                            self.search_debounce = Some(Instant::now());
                        }
                        InputAction::ResumeSelected => {
                            if let Some(result) = self.results.get(self.selected_index) {
                                // Restore the terminal before launching claude --resume
                                ui::restore_terminal()?;
                                crate::session::resume_session(result)?;
                                return Ok(());
                            } else {
                                self.status_message =
                                    Some("No session selected to resume.".to_string());
                            }
                        }
                        InputAction::Quit => {
                            self.should_quit = true;
                        }
                    }
                }
            }
        }
    }

    async fn trigger_search(&mut self) {
        let query = self.search_input.trim();

        // If query is empty, search with a wildcard to get recent sessions
        let search_query = if query.is_empty() { "*" } else { query };

        match self.qmd.search(search_query, 20).await {
            Ok(results) => {
                self.results = results;
                self.status_message = None;
                // Clamp selected index
                if self.selected_index >= self.results.len() && !self.results.is_empty() {
                    self.selected_index = self.results.len() - 1;
                }
                if self.results.is_empty() {
                    self.selected_index = 0;
                }
                // Load preview for the selected result
                self.load_preview();
            }
            Err(e) => {
                self.status_message = Some(format!("Search error: {e}"));
                self.results.clear();
                self.selected_index = 0;
                self.preview_content = None;
            }
        }
    }

    pub fn load_preview(&mut self) {
        if let Some(result) = self.results.get(self.selected_index) {
            if let Some(ref file_path) = result.file_path {
                match std::fs::read_to_string(file_path) {
                    Ok(content) => {
                        self.preview_content = Some(content);
                    }
                    Err(e) => {
                        self.preview_content =
                            Some(format!("Error reading preview: {e}"));
                    }
                }
            } else {
                self.preview_content = Some("No preview available.".to_string());
            }
        } else {
            self.preview_content = None;
        }
    }

    pub fn select_next(&mut self) {
        if !self.results.is_empty() {
            self.selected_index = (self.selected_index + 1).min(self.results.len() - 1);
            self.load_preview();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.results.is_empty() {
            self.selected_index = self.selected_index.saturating_sub(1);
            self.load_preview();
        }
    }
}
