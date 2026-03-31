use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, MouseEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::config::Config;
use crate::exporter::markdown::SessionDocument;
use crate::qmd::{QmdClient, SearchResult};

use super::folder_tree::FolderTree;
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

    // Daemon status
    pub config: Config,
    pub daemon_status: String,
    daemon_check_interval: Option<Instant>,

    // Folder sidebar state
    pub folder_tree: FolderTree,
    /// Index into visible_rows() for the folder cursor.
    pub folder_cursor: usize,
    /// The project path prefix used as the active filter, or None for "All".
    pub active_filter: Option<String>,
    /// Indices into `self.results` that match the active filter.
    pub filtered_indices: Vec<usize>,
    /// Which pane has focus when in browse mode.
    pub focus: FocusPane,

    search_debounce: Option<Instant>,
    search_generation: u64,
    search_rx: mpsc::UnboundedReceiver<SearchResponse>,
    search_tx: mpsc::UnboundedSender<SearchResponse>,
    /// Handle to the in-flight search task. Aborted when a new search is spawned.
    inflight_search: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Search,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Folders,
    Results,
}

impl App {
    pub fn new(qmd: QmdClient, config: Config) -> Self {
        let (search_tx, search_rx) = mpsc::unbounded_channel();
        let daemon_status = Self::check_daemon_status(&config);
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
            config,
            daemon_status,
            daemon_check_interval: Some(Instant::now()),
            folder_tree: FolderTree::build(&[]),
            folder_cursor: 0,
            active_filter: None,
            filtered_indices: Vec::new(),
            focus: FocusPane::Results,
            search_debounce: None,
            search_generation: 0,
            search_rx,
            search_tx,
            inflight_search: None,
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
                    continue;
                }
                self.searching = false;
                self.last_search_time = Some(response.elapsed);
                match response.result {
                    Ok(results) => {
                        self.result_count = results.len();
                        self.results = results;
                        self.status_message = None;
                        self.rebuild_folders();
                        self.apply_filter();
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

            // Refresh daemon status every 30 seconds
            if let Some(last_check) = self.daemon_check_interval {
                if Instant::now().duration_since(last_check) >= Duration::from_secs(30) {
                    self.daemon_status = Self::check_daemon_status(&self.config);
                    self.daemon_check_interval = Some(Instant::now());
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
                                if let Some(&ri) = self.filtered_indices.get(self.selected_index) {
                                    if let Some(result) = self.results.get(ri) {
                                        ui::restore_terminal()?;
                                        crate::session::resume_session(result)?;
                                        return Ok(());
                                    }
                                }
                                self.status_message =
                                    Some("No session selected to resume.".to_string());
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
        // Abort any in-flight search so it doesn't queue up in QMD
        // and block the new request.
        if let Some(handle) = self.inflight_search.take() {
            handle.abort();
            tracing::debug!("Aborted in-flight search");
        }

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

        self.inflight_search = Some(tokio::spawn(async move {
            let start = Instant::now();
            let result = qmd.search(&search_query, 20).await.map_err(|e| format!("{e}"));
            let _ = tx.send(SearchResponse {
                generation,
                result,
                elapsed: start.elapsed(),
            });
        }));
    }

    pub fn load_preview(&mut self) {
        self.preview_scroll = 0;
        let result = self
            .filtered_indices
            .get(self.selected_index)
            .and_then(|&ri| self.results.get(ri));
        if let Some(result) = result {
            if let Some(ref file_path) = result.file_path {
                match std::fs::read_to_string(file_path) {
                    Ok(content) => {
                        let normalized = SessionDocument::parse(&content)
                            .map(|doc| doc.render_preview())
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
        if !self.filtered_indices.is_empty() {
            self.selected_index =
                (self.selected_index + 1).min(self.filtered_indices.len() - 1);
            self.load_preview();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected_index = self.selected_index.saturating_sub(1);
            self.load_preview();
        }
    }

    /// Rebuild the folder tree from current results.
    fn rebuild_folders(&mut self) {
        let paths: Vec<Option<String>> = self
            .results
            .iter()
            .map(|r| r.project_path.clone())
            .collect();
        self.folder_tree = FolderTree::build(&paths);
        // Keep folder_cursor in bounds.
        let visible = self.folder_tree.visible_rows();
        // +1 for the "All" row which is virtual (not in the tree).
        let max = visible.len(); // 0 = "All", 1..=len = tree rows
        if self.folder_cursor > max {
            self.folder_cursor = 0;
        }
    }

    /// Recompute filtered_indices based on the active folder filter.
    pub fn apply_filter(&mut self) {
        self.filtered_indices = match &self.active_filter {
            None => (0..self.results.len()).collect(),
            Some(prefix) => self
                .results
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    r.project_path.as_ref().is_some_and(|p| p.starts_with(prefix.as_str()))
                })
                .map(|(i, _)| i)
                .collect(),
        };
        // Reset result selection.
        if self.filtered_indices.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.filtered_indices.len() {
            self.selected_index = self.filtered_indices.len() - 1;
        }
    }

    /// Set the active folder filter from the currently selected folder row.
    pub fn set_folder_filter(&mut self) {
        let visible = self.folder_tree.visible_rows();
        if self.folder_cursor == 0 {
            // "All" row
            self.active_filter = None;
        } else if let Some(row) = visible.get(self.folder_cursor - 1) {
            self.active_filter = Some(row.full_path.clone());
        }
        self.apply_filter();
        self.load_preview();
    }

    pub fn folder_select_next(&mut self) {
        let visible = self.folder_tree.visible_rows();
        let max = visible.len(); // 0=All, 1..=len
        if self.folder_cursor < max {
            self.folder_cursor += 1;
        }
    }

    pub fn folder_select_previous(&mut self) {
        self.folder_cursor = self.folder_cursor.saturating_sub(1);
    }

    pub fn folder_expand(&mut self) {
        let visible = self.folder_tree.visible_rows();
        if self.folder_cursor > 0 {
            if let Some(row) = visible.get(self.folder_cursor - 1) {
                if row.has_children {
                    let path = row.tree_path.clone();
                    self.folder_tree.expand(&path);
                }
            }
        }
    }

    pub fn folder_collapse(&mut self) {
        let visible = self.folder_tree.visible_rows();
        if self.folder_cursor > 0 {
            if let Some(row) = visible.get(self.folder_cursor - 1) {
                let path = row.tree_path.clone();
                self.folder_tree.collapse(&path);
                // Keep cursor in bounds after collapse.
                let new_visible = self.folder_tree.visible_rows();
                let max = new_visible.len();
                if self.folder_cursor > max {
                    self.folder_cursor = max;
                }
            }
        }
    }

    fn check_daemon_status(config: &Config) -> String {
        let running = crate::watcher::is_running(config);

        let session_count = std::fs::read_dir(config.export_dir())
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
                    .count()
            })
            .unwrap_or(0);

        let sources = config.claude_projects_dirs().len();

        let daemon_label = if !running {
            "○ daemon stopped"
        } else if config.is_indexing() {
            "● indexing…"
        } else {
            "● idle"
        };
        format!(
            "{daemon_label} · {session_count} sessions · {sources} sources"
        )
    }
}
