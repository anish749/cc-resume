use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::qmd::{QmdClient, SearchResult};

use super::input::{self, InputAction};
use super::ui;

/// Message sent back from a background search task.
enum SearchResponse {
    Results(Vec<SearchResult>),
    Error(String),
}

pub struct App {
    pub qmd: Arc<QmdClient>,
    pub search_input: String,
    pub cursor_position: usize,
    pub results: Vec<SearchResult>,
    pub selected_index: usize,
    pub preview_content: Option<String>,
    pub mode: AppMode,
    pub should_quit: bool,
    pub search_debounce: Option<Instant>,
    pub status_message: Option<String>,
    pub searching: bool,
    search_rx: mpsc::UnboundedReceiver<SearchResponse>,
    search_tx: mpsc::UnboundedSender<SearchResponse>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Search,
    Results,
}

impl App {
    pub fn new(qmd: QmdClient) -> Self {
        let (search_tx, search_rx) = mpsc::unbounded_channel();
        Self {
            qmd: Arc::new(qmd),
            search_input: String::new(),
            cursor_position: 0,
            results: Vec::new(),
            selected_index: 0,
            preview_content: None,
            mode: AppMode::Search,
            should_quit: false,
            search_debounce: None,
            status_message: None,
            searching: false,
            search_rx,
            search_tx,
        }
    }

    pub async fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        // Kick off an initial search
        self.spawn_search();

        loop {
            terminal.draw(|f| ui::draw(f, self))?;

            if self.should_quit {
                return Ok(());
            }

            // Check for completed background search results (non-blocking)
            while let Ok(response) = self.search_rx.try_recv() {
                self.searching = false;
                match response {
                    SearchResponse::Results(results) => {
                        self.results = results;
                        self.status_message = None;
                        if self.selected_index >= self.results.len() && !self.results.is_empty() {
                            self.selected_index = self.results.len() - 1;
                        }
                        if self.results.is_empty() {
                            self.selected_index = 0;
                        }
                        self.load_preview();
                    }
                    SearchResponse::Error(e) => {
                        self.status_message = Some(format!("Search error: {e}"));
                        self.results.clear();
                        self.selected_index = 0;
                        self.preview_content = None;
                    }
                }
            }

            // Check if a debounced search should fire
            if let Some(debounce_instant) = self.search_debounce {
                if Instant::now().duration_since(debounce_instant) >= Duration::from_millis(300) {
                    self.search_debounce = None;
                    self.spawn_search();
                }
            }

            // Poll for crossterm events with a 50ms timeout (responsive UI)
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    let action = input::handle_key_event(self, key);
                    match action {
                        InputAction::None => {}
                        InputAction::SearchChanged => {
                            self.search_debounce = Some(Instant::now());
                        }
                        InputAction::ResumeSelected => {
                            if let Some(result) = self.results.get(self.selected_index) {
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

    /// Spawn a search on a background tokio task so the UI stays responsive.
    fn spawn_search(&mut self) {
        let query = self.search_input.trim().to_string();
        let search_query = if query.is_empty() {
            "*".to_string()
        } else {
            query
        };

        self.searching = true;
        let qmd = Arc::clone(&self.qmd);
        let tx = self.search_tx.clone();

        tokio::spawn(async move {
            let response = match qmd.search(&search_query, 20).await {
                Ok(results) => SearchResponse::Results(results),
                Err(e) => SearchResponse::Error(format!("{e}")),
            };
            let _ = tx.send(response);
        });
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
