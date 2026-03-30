use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, MouseEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::exporter::markdown::SessionDocument;
use crate::qmd::{QmdClient, SearchResult};

use super::input::{self, InputAction};
use super::ui;

struct SearchResponse {
    generation: u64,
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
    pub preview_scroll: u16,

    search_debounce: Option<Instant>,
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
            preview_scroll: 0,
            mode: AppMode::Search,
            should_quit: false,
            status_message: None,
            searching: false,
            last_search_time: None,
            result_count: 0,
            search_debounce: None,
            search_generation: 0,
            search_rx,
            search_tx,
        }
    }

    pub async fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        self.spawn_search();

        loop {
            terminal.draw(|f| ui::draw(f, self))?;

            if self.should_quit {
                return Ok(());
            }

            // Collect completed search results (non-blocking)
            while let Ok(response) = self.search_rx.try_recv() {
                if response.generation < self.search_generation {
                    tracing::debug!(
                        "Discarding stale result (gen {} < {})",
                        response.generation,
                        self.search_generation
                    );
                    continue;
                }
                self.searching = false;
                self.last_search_time = Some(response.elapsed);
                match response.result {
                    Ok(results) => {
                        tracing::debug!(
                            "Got {} results in {:.1}s",
                            results.len(),
                            response.elapsed.as_secs_f64()
                        );
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

            // Fire search 300ms after last keystroke
            if let Some(debounce_instant) = self.search_debounce {
                if Instant::now().duration_since(debounce_instant) >= Duration::from_millis(300) {
                    self.search_debounce = None;
                    self.spawn_search();
                }
            }

            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) => {
                        let action = input::handle_key_event(self, key);
                        match action {
                            InputAction::None => {}
                            InputAction::SearchChanged => {
                                self.search_debounce = Some(Instant::now());
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
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollDown => self.scroll_preview_down(3),
                        MouseEventKind::ScrollUp => self.scroll_preview_up(3),
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }

    fn spawn_search(&mut self) {
        let query = self.search_input.trim().to_string();
        let search_query = if query.is_empty() {
            "*".to_string()
        } else {
            query
        };

        self.searching = true;
        let generation = self.search_generation;
        tracing::debug!("Spawning search: query={search_query:?} gen={generation}");
        let qmd = Arc::clone(&self.qmd);
        let tx = self.search_tx.clone();

        tokio::spawn(async move {
            let start = Instant::now();
            let result = qmd.search(&search_query, 20).await.map_err(|e| format!("{e}"));
            let _ = tx.send(SearchResponse {
                generation,
                result,
                elapsed: start.elapsed(),
            });
        });
    }

    pub fn load_preview(&mut self) {
        self.preview_scroll = 0;
        if let Some(result) = self.results.get(self.selected_index) {
            if let Some(ref file_path) = result.file_path {
                match std::fs::read_to_string(file_path) {
                    Ok(content) => {
                        // Re-render through the typed document so frontmatter
                        // field order always matches the struct definition,
                        // regardless of what's on disk.
                        let normalized = SessionDocument::parse(&content)
                            .map(|doc| doc.render())
                            .unwrap_or(content);
                        self.preview_content = Some(normalized);
                    }
                    Err(e) => self.preview_content = Some(format!("Error reading preview: {e}")),
                }
            } else {
                self.preview_content = Some("No preview available.".to_string());
            }
        } else {
            self.preview_content = None;
        }
    }

    pub fn scroll_preview_down(&mut self, amount: u16) {
        self.preview_scroll = self.preview_scroll.saturating_add(amount);
    }

    pub fn scroll_preview_up(&mut self, amount: u16) {
        self.preview_scroll = self.preview_scroll.saturating_sub(amount);
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
