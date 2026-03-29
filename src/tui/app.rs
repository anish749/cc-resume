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

struct SearchResponse {
    generation: u64,
    is_deep: bool,
    result: std::result::Result<Vec<SearchResult>, String>,
    elapsed: Duration,
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
    pub status_message: Option<String>,
    pub searching: bool,
    pub last_search_time: Option<Duration>,
    pub result_count: usize,
    pub is_deep_result: bool,

    last_keystroke: Option<Instant>,
    fast_search_fired: bool,
    deep_search_fired: bool,
    search_generation: u64,

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
            status_message: None,
            searching: false,
            last_search_time: None,
            result_count: 0,
            is_deep_result: false,
            last_keystroke: None,
            fast_search_fired: false,
            deep_search_fired: false,
            search_generation: 0,
            search_rx,
            search_tx,
        }
    }

    pub async fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        self.spawn_search(false);

        loop {
            terminal.draw(|f| ui::draw(f, self))?;

            if self.should_quit {
                return Ok(());
            }

            // Collect completed search results (non-blocking)
            while let Ok(response) = self.search_rx.try_recv() {
                // Only accept results from the current generation.
                // Fast search (gen N) and deep search (gen N) are both valid.
                // But if user typed again (gen N+1), discard gen N results.
                if response.generation < self.search_generation {
                    continue;
                }
                // If we already have deep results, don't downgrade to fast
                if self.is_deep_result && !response.is_deep && response.generation == self.search_generation {
                    continue;
                }
                self.searching = self.searching && !response.is_deep;
                self.last_search_time = Some(response.elapsed);
                self.is_deep_result = response.is_deep;
                match response.result {
                    Ok(results) => {
                        self.result_count = results.len();
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
                    Err(e) => {
                        self.status_message = Some(format!("Search error: {e}"));
                        self.results.clear();
                        self.result_count = 0;
                        self.selected_index = 0;
                        self.preview_content = None;
                    }
                }
            }

            // Debounce: fast search at 300ms, deep search at 2s
            if let Some(last) = self.last_keystroke {
                let elapsed = Instant::now().duration_since(last);
                if !self.fast_search_fired && elapsed >= Duration::from_millis(300) {
                    self.fast_search_fired = true;
                    self.spawn_search(false);
                }
                if !self.deep_search_fired && elapsed >= Duration::from_secs(2) {
                    self.deep_search_fired = true;
                    self.spawn_search(true);
                }
            }

            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    let action = input::handle_key_event(self, key);
                    match action {
                        InputAction::None => {}
                        InputAction::SearchChanged => {
                            self.last_keystroke = Some(Instant::now());
                            self.fast_search_fired = false;
                            self.deep_search_fired = false;
                            self.is_deep_result = false;
                            // Bump generation so in-flight searches get discarded
                            self.search_generation += 1;
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

    fn spawn_search(&mut self, deep: bool) {
        let query = self.search_input.trim().to_string();
        let search_query = if query.is_empty() {
            "*".to_string()
        } else {
            query
        };

        self.searching = true;
        let generation = self.search_generation;
        let qmd = Arc::clone(&self.qmd);
        let tx = self.search_tx.clone();

        tokio::spawn(async move {
            let start = Instant::now();
            let search_result = if deep {
                qmd.deep_search(&search_query, 20).await
            } else {
                qmd.fast_search(&search_query, 20).await
            };
            let result = search_result.map_err(|e| format!("{e}"));
            let _ = tx.send(SearchResponse {
                generation,
                is_deep: deep,
                result,
                elapsed: start.elapsed(),
            });
        });
    }

    pub fn load_preview(&mut self) {
        if let Some(result) = self.results.get(self.selected_index) {
            if let Some(ref file_path) = result.file_path {
                match std::fs::read_to_string(file_path) {
                    Ok(content) => self.preview_content = Some(content),
                    Err(e) => self.preview_content = Some(format!("Error reading preview: {e}")),
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
