//! Terminal user interface for mentisdbd.
//!
//! Provides a live three-pane TUI with scrollable tables for chains, agents,
//! and skills, plus an event log and agent primer. Uses the standard 8-color
//! ANSI palette so colors adapt to dark and light terminal backgrounds.

#![allow(missing_docs)]

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use log::{Level, Record};
use ratatui::{
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, TableState, Tabs, Wrap,
    },
    Frame, Terminal,
};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A custom logger that routes log records into the TUI's log buffer via a channel,
/// preventing raw stderr output from corrupting the ratatui alternate screen.
pub struct TuiLogger {
    tx: mpsc::Sender<String>,
}

impl TuiLogger {
    pub fn new(tx: mpsc::Sender<String>) -> Self {
        Self { tx }
    }
}

impl log::Log for TuiLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let level_color = match record.level() {
                Level::Error => "ERR",
                Level::Warn => "WRN",
                Level::Info => "INF",
                Level::Debug => "DBG",
                Level::Trace => "TRC",
            };
            let target = record.target();
            let msg = record.args().to_string();
            let line = format!("[{level_color}] {target}: {msg}");
            let _ = self.tx.send(line);
        }
    }

    fn flush(&self) {}
}

/// Initialize the TUI-aware logger. Returns the receiver end of the channel
/// that the TUI event loop should drain to populate the log panel.
pub fn init_tui_logger() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let logger = TuiLogger::new(tx);
    let _ = log::set_boxed_logger(Box::new(logger));
    log::set_max_level(log::LevelFilter::Info);
    rx
}

/// Standard 8-color palette — adapts to the terminal's own color theme.
/// On dark backgrounds these render as bright/saturated; on light backgrounds
/// they render as darker/muted — readable either way.
/// Never use Color::Indexed(N) or Color::Rgb(r,g,b) — those are fixed-palette
/// and become invisible or harsh on light/white backgrounds.
const HIGHLIGHT_STYLE: Style = Style::new()
    .add_modifier(Modifier::REVERSED)
    .add_modifier(Modifier::BOLD);

#[derive(Clone)]
pub struct ChainInfo {
    pub key: String,
    pub version: u32,
    pub adapter: String,
    pub thoughts: usize,
    pub agents: usize,
    pub storage_path: String,
}

#[derive(Clone)]
pub struct AgentInfo {
    pub chain_key: String,
    pub name: String,
    pub id: String,
    pub status: String,
    pub memories: usize,
    pub description: String,
}

#[derive(Clone)]
pub struct SkillInfo {
    pub name: String,
    pub status: String,
    pub versions: usize,
    pub tags: Vec<String>,
    pub uploaded_by: String,
}

/// Which pane currently has keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusedPane {
    TopLeft,
    TopRight,
    Prime,
    Tables,
    Logs,
}

impl FocusedPane {
    fn next(self) -> Self {
        match self {
            Self::TopLeft => Self::TopRight,
            Self::TopRight => Self::Prime,
            Self::Prime => Self::Tables,
            Self::Tables => Self::Logs,
            Self::Logs => Self::TopLeft,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::TopLeft => Self::Logs,
            Self::TopRight => Self::TopLeft,
            Self::Prime => Self::TopRight,
            Self::Tables => Self::Prime,
            Self::Logs => Self::Tables,
        }
    }
}

pub struct TuiState {
    pub version: String,
    pub started: bool,
    pub startup_status: String,
    /// If startup failed, this holds the error shown in a full-screen red
    /// overlay. The TUI stays running so the user can read the error and logs.
    pub startup_error: Option<String>,
    pub config_lines: Vec<String>,
    pub migration_lines: Vec<String>,
    pub endpoint_lines: Vec<String>,
    pub tls_info_lines: Vec<String>,
    pub chains: Vec<ChainInfo>,
    pub agents: Vec<AgentInfo>,
    pub skills: Vec<SkillInfo>,
    pub log_lines: Vec<String>,
    pub primer_text: String,
    pub chain_count: usize,
    pub tab_index: usize,
    pub tab_titles: Vec<&'static str>,
    pub chain_table_state: TableState,
    pub agent_table_state: TableState,
    pub skill_table_state: TableState,
    pub log_scroll: usize,
    pub log_auto_scroll: bool,
    pub left_scroll: usize,
    pub right_scroll: usize,
    pub focused_pane: FocusedPane,
    pub should_quit: bool,
    /// When Some, a modal update dialog is shown. Contains
    /// (current_version, latest_display, release_url).
    pub update_dialog: Option<(String, String, String)>,
    /// Sender to reply to the update dialog. The background startup
    /// thread waits on the paired receiver for the user's choice.
    pub update_response_tx: Option<mpsc::Sender<bool>>,
    /// Temporary toast shown after a clipboard copy. Cleared after 2 s.
    pub toast: Option<(String, Instant)>,
    /// Active drag-select gesture: start and current screen positions.
    pub drag_start: Option<Position>,
    pub drag_current: Option<Position>,
    /// Layout areas cached during the last render pass for mouse hit-testing.
    last_top_left_area: Rect,
    last_top_right_area: Rect,
    last_prime_area: Rect,
    last_tables_area: Rect,
    last_tabs_area: Rect,
    last_logs_area: Rect,
}

impl TuiState {
    pub fn new(version: &str) -> Self {
        Self {
            version: version.to_string(),
            started: false,
            startup_status: "Starting…".to_string(),
            startup_error: None,
            config_lines: Vec::new(),
            migration_lines: Vec::new(),
            endpoint_lines: Vec::new(),
            tls_info_lines: Vec::new(),
            chains: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            log_lines: Vec::new(),
            primer_text: String::new(),
            chain_count: 0,
            tab_index: 0,
            tab_titles: vec!["Chains", "Agents", "Skills"],
            chain_table_state: TableState::default(),
            agent_table_state: TableState::default(),
            skill_table_state: TableState::default(),
            log_scroll: 0,
            log_auto_scroll: true,
            left_scroll: 0,
            right_scroll: 0,
            focused_pane: FocusedPane::Tables,
            should_quit: false,
            update_dialog: None,
            update_response_tx: None,
            toast: None,
            drag_start: None,
            drag_current: None,
            last_top_left_area: Rect::default(),
            last_top_right_area: Rect::default(),
            last_prime_area: Rect::default(),
            last_tables_area: Rect::default(),
            last_tabs_area: Rect::default(),
            last_logs_area: Rect::default(),
        }
    }

    pub fn add_log(&mut self, line: String) {
        self.log_lines.push(line);
        if self.log_lines.len() > 5000 {
            let keep = self.log_lines.len() - 4000;
            self.log_lines.drain(0..keep);
        }
        if self.log_auto_scroll {
            // Panel renders newest-first; position 0 = newest. Auto-scroll
            // keeps the viewport pinned to the top (latest entries).
            self.log_scroll = 0;
        }
    }

    pub fn scroll_left_up(&mut self) {
        self.left_scroll = self.left_scroll.saturating_sub(1);
    }

    pub fn scroll_left_down(&mut self) {
        let content_lines = self.left_content_len();
        let visible = self.last_top_left_area.height.saturating_sub(2) as usize;
        let max = content_lines.saturating_sub(visible);
        self.left_scroll = (self.left_scroll + 1).min(max);
    }

    pub fn scroll_right_up(&mut self) {
        self.right_scroll = self.right_scroll.saturating_sub(1);
    }

    pub fn scroll_right_down(&mut self) {
        let content_lines = self.endpoint_lines.len() + self.tls_info_lines.len() + 3;
        let visible = self.last_top_right_area.height.saturating_sub(2) as usize;
        let max = content_lines.saturating_sub(visible);
        self.right_scroll = (self.right_scroll + 1).min(max);
    }

    pub fn scroll_logs_up(&mut self) {
        // Newest-first display: ↑ moves toward newer entries (smaller offset).
        self.log_scroll = self.log_scroll.saturating_sub(1);
        if self.log_scroll == 0 {
            self.log_auto_scroll = true;
        }
    }

    pub fn scroll_logs_down(&mut self) {
        // Newest-first display: ↓ moves toward older entries (larger offset).
        self.log_auto_scroll = false;
        let visible = self.last_logs_area.height.saturating_sub(2) as usize;
        let max = self.log_lines.len().saturating_sub(visible);
        self.log_scroll = (self.log_scroll + 1).min(max);
    }

    /// Number of content lines in the top-left panel (banner + version + config + migrations).
    fn left_content_len(&self) -> usize {
        let banner_lines = MENTIS_BANNER.lines().count();
        // banner + version + status + blank + "Configuration:" header + config + migrations
        banner_lines + 2 + 1 + 1 + self.config_lines.len() + self.migration_lines.len()
    }

    pub fn select_chain_prev(&mut self) {
        cycle_table_selection(&mut self.chain_table_state, self.chains.len(), false);
    }

    pub fn select_chain_next(&mut self) {
        cycle_table_selection(&mut self.chain_table_state, self.chains.len(), true);
    }

    pub fn select_agent_prev(&mut self) {
        cycle_table_selection(&mut self.agent_table_state, self.agents.len(), false);
    }

    pub fn select_agent_next(&mut self) {
        cycle_table_selection(&mut self.agent_table_state, self.agents.len(), true);
    }

    pub fn select_skill_prev(&mut self) {
        cycle_table_selection(&mut self.skill_table_state, self.skills.len(), false);
    }

    pub fn select_skill_next(&mut self) {
        cycle_table_selection(&mut self.skill_table_state, self.skills.len(), true);
    }

    /// Text of the currently focused/selected item — used for clipboard copy.
    pub fn selected_item_text(&self) -> Option<String> {
        match self.focused_pane {
            FocusedPane::Prime => {
                if self.primer_text.is_empty() {
                    None
                } else {
                    Some(self.primer_text.clone())
                }
            }
            FocusedPane::Tables => match self.tab_index {
                0 => self
                    .chain_table_state
                    .selected()
                    .and_then(|i| self.chains.get(i))
                    .map(|c| c.key.clone()),
                1 => self
                    .agent_table_state
                    .selected()
                    .and_then(|i| self.agents.get(i))
                    .map(|a| a.id.clone()),
                2 => self
                    .skill_table_state
                    .selected()
                    .and_then(|i| self.skills.get(i))
                    .map(|s| s.name.clone()),
                _ => None,
            },
            FocusedPane::Logs => {
                // Copy the visible log lines (newest-first, as rendered).
                if self.log_lines.is_empty() {
                    None
                } else {
                    let visible: Vec<&str> = self
                        .log_lines
                        .iter()
                        .rev()
                        .skip(self.log_scroll)
                        .take(50)
                        .map(|s| s.as_str())
                        .collect();
                    Some(visible.join("\n"))
                }
            }
            _ => None,
        }
    }

    /// Rect of the pane that anchors the copy toast.
    pub fn toast_anchor(&self) -> Rect {
        match self.focused_pane {
            FocusedPane::Prime => self.last_prime_area,
            FocusedPane::Tables => self.last_tables_area,
            FocusedPane::Logs => self.last_logs_area,
            FocusedPane::TopLeft => self.last_top_left_area,
            FocusedPane::TopRight => self.last_top_right_area,
        }
    }

    /// Request an in-TUI update dialog. Blocks until the user responds
    /// (y/N/Enter/Esc) or the TUI is quit. Returns `None` if the TUI
    /// was quit before the user made a choice.
    pub fn request_update_dialog(
        state: &Arc<std::sync::Mutex<TuiState>>,
        current_version: &str,
        latest_display: &str,
        release_url: &str,
    ) -> Option<bool> {
        let (tx, rx) = mpsc::channel();
        {
            let mut s = state.lock().unwrap();
            s.update_dialog = Some((
                current_version.to_string(),
                latest_display.to_string(),
                release_url.to_string(),
            ));
            s.update_response_tx = Some(tx);
        }
        rx.recv().ok()
    }
}

/// Cycles the selected row in a [`TableState`], wrapping around at both ends.
fn cycle_table_selection(table_state: &mut TableState, len: usize, forward: bool) {
    if len == 0 {
        return;
    }
    let i = match table_state.selected() {
        Some(i) => {
            if forward {
                if i >= len - 1 {
                    0
                } else {
                    i + 1
                }
            } else if i == 0 {
                len - 1
            } else {
                i - 1
            }
        }
        None => 0,
    };
    table_state.select(Some(i));
}

const MENTIS_BANNER: &str = "\
███╗   ███╗███████╗███╗   ██╗████████╗██╗███████╗ ██████╗ ██████╗ 
████╗ ████║██╔════╝████╗  ██║╚══██╔══╝██║██╔════╝ ██╔══██╗██╔══██╗
██╔████╔██║█████╗  ██╔██╗ ██║   ██║   ██║███████╗ ██║  ██║██████╔╝
██║╚██╔╝██║██╔══╝  ██║╚██╗██║   ██║   ██║╚════██║ ██║  ██║██╔══██╗
██║ ╚═╝ ██║███████╗██║ ╚████║   ██║   ██║███████║ ██████╔╝██████╔╝
╚═╝     ╚═╝╚══════╝╚═╝  ╚═══╝   ╚═╝   ╚═╝╚══════╝ ╚═════╝ ╚═════╝ ";

fn ui(frame: &mut Frame, state: &mut TuiState) {
    let full_area = frame.area();

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(17),
            Constraint::Length(5),
            Constraint::Max(14),
            Constraint::Min(14),
            Constraint::Length(1),
        ])
        .split(full_area);

    let top_area = main_layout[0];
    let prime_area = main_layout[1];
    let tables_area = main_layout[2];
    let logs_area = main_layout[3];
    let hint_area = main_layout[4];

    let top_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(85), Constraint::Percentage(100)])
        .split(top_area);

    let top_left = top_split[0];
    let top_right = top_split[1];

    // Cache layout areas for mouse hit-testing and scroll clamping.
    state.last_top_left_area = top_left;
    state.last_top_right_area = top_right;
    state.last_tables_area = tables_area;
    state.last_prime_area = prime_area;
    state.last_logs_area = logs_area;

    render_top_left(frame, state, top_left);
    render_top_right(frame, state, top_right);
    render_prime(frame, state, prime_area);
    render_tables(frame, state, tables_area);
    render_logs(frame, state, logs_area);
    render_hint_bar(frame, state, hint_area);

    // Overlays: at most one modal is shown at a time.
    if let Some(ref err) = state.startup_error {
        render_crash_overlay(frame, err, full_area);
    } else if state.update_dialog.is_some() {
        render_update_dialog_overlay(frame, state, full_area);
    } else if !state.started {
        render_startup_overlay(frame, state, full_area);
    }

    // Selection highlight: invert the style of every cell in the drag rect.
    if let (Some(start), Some(end)) = (state.drag_start, state.drag_current) {
        let y1 = start.y.min(end.y);
        let y2 = start.y.max(end.y);
        let x1 = start.x.min(end.x);
        let x2 = start.x.max(end.x);
        if x2 >= x1 && y2 >= y1 {
            let sel = Rect {
                x: x1,
                y: y1,
                width: x2 - x1 + 1,
                height: y2 - y1 + 1,
            };
            frame.buffer_mut().set_style(sel, Style::default().add_modifier(Modifier::REVERSED));
        }
    }

    if let Some((ref msg, _)) = state.toast {
        let anchor = state.toast_anchor();
        render_toast(frame, msg, anchor);
    }
}

/// Returns the border style for a pane — highlighted when focused.
fn border_style_for(state: &TuiState, pane: FocusedPane) -> Style {
    if state.focused_pane == pane {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_top_left(frame: &mut Frame, state: &TuiState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for banner_line in MENTIS_BANNER.lines() {
        lines.push(Line::from(Span::styled(
            banner_line.to_string(),
            Style::default().fg(Color::Green),
        )));
    }

    lines.push(Line::from(Span::styled(
        format!("mentisdb v{}", state.version),
        Style::default().add_modifier(Modifier::BOLD),
    )));

    if state.started {
        lines.push(Line::from(Span::styled(
            "mentisdbd running",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "mentisdbd starting…",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Configuration:",
        Style::default().add_modifier(Modifier::BOLD),
    )));

    for config_line in &state.config_lines {
        lines.push(Line::from(config_line.clone()));
    }

    if !state.migration_lines.is_empty() {
        for m in &state.migration_lines {
            lines.push(Line::from(m.clone()));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style_for(state, FocusedPane::TopLeft));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((state.left_scroll as u16, 0));
    frame.render_widget(paragraph, area);

    let total = state.left_content_len();
    if total + 2 > area.height as usize {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        let mut scrollbar_state = ScrollbarState::new(total).position(state.left_scroll);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn render_top_right(frame: &mut Frame, state: &TuiState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    // Endpoints first — most important to see at a glance.
    lines.push(Line::from(Span::styled(
        "Endpoints",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for ep_line in &state.endpoint_lines {
        lines.push(Line::from(ep_line.clone()));
    }

    if !state.tls_info_lines.is_empty() {
        lines.push(Line::from(""));
        for tls_line in &state.tls_info_lines {
            lines.push(Line::from(tls_line.clone()));
        }
    }

    let total_content = lines.len();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style_for(state, FocusedPane::TopRight))
        .title(" Endpoints & TLS ");

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((state.right_scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);

    if total_content + 2 > area.height as usize {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        let mut scrollbar_state = ScrollbarState::new(total_content).position(state.right_scroll);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn render_prime(frame: &mut Frame, state: &TuiState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        "Prime your agent — paste into your AI chat:",
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let primer_span = Span::styled(format!(" {} ", state.primer_text), HIGHLIGHT_STYLE);
    lines.push(Line::from(primer_span));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style_for(state, FocusedPane::Prime));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_tables(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);

    let tabs_area = layout[0];
    let table_area = layout[1];

    // Cache tabs area for click detection.
    state.last_tabs_area = tabs_area;

    let tabs = Tabs::new(state.tab_titles.iter().copied())
        .select(state.tab_index)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style_for(state, FocusedPane::Tables)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|");

    frame.render_widget(tabs, tabs_area);

    match state.tab_index {
        0 => render_chain_table(frame, state, table_area),
        1 => render_agent_table(frame, state, table_area),
        2 => render_skill_table(frame, state, table_area),
        _ => {}
    }
}

fn render_chain_table(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let header = Row::new(vec![
        "Chain Key",
        "Ver",
        "Adapter",
        "Thoughts",
        "Agents",
        "Storage Location",
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = state
        .chains
        .iter()
        .map(|c| {
            Row::new(vec![
                c.key.clone(),
                c.version.to_string(),
                c.adapter.clone(),
                c.thoughts.to_string(),
                c.agents.to_string(),
                c.storage_path.clone(),
            ])
        })
        .collect();

    // Size the Chain Key column to fit the longest key (min 10, header "Chain Key" = 9).
    let key_col_width = state
        .chains
        .iter()
        .map(|c| c.key.len())
        .max()
        .unwrap_or(9)
        .max(9) as u16
        + 1;

    let widths = [
        Constraint::Length(key_col_width),
        Constraint::Length(5),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Percentage(100),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(format!(" Chains  ({}) ", state.chain_count)),
        )
        .row_highlight_style(HIGHLIGHT_STYLE);

    frame.render_stateful_widget(table, area, &mut state.chain_table_state);

    if !state.chains.is_empty() {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        let mut scrollbar_state = ScrollbarState::new(state.chains.len())
            .position(state.chain_table_state.selected().unwrap_or(0));
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn render_agent_table(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let header = Row::new(vec!["Name", "ID", "Status", "Memories", "Description"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let mut rows: Vec<Row> = Vec::new();
    let mut current_chain = String::new();

    for agent in &state.agents {
        if agent.chain_key != current_chain {
            current_chain = agent.chain_key.clone();
            rows.push(
                Row::new(vec![
                    Cell::from(Span::styled(
                        format!(" {}", current_chain),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                ])
                .height(1)
                .style(Style::default().fg(Color::DarkGray)),
            );
        }

        rows.push(Row::new(vec![
            agent.name.clone(),
            agent.id.clone(),
            agent.status.clone(),
            agent.memories.to_string(),
            if agent.description.is_empty() {
                "—".to_string()
            } else {
                agent.description.clone()
            },
        ]));
    }

    let widths = [
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Percentage(100),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(format!(" Agent Registry  ({}) ", state.agents.len())),
        )
        .row_highlight_style(HIGHLIGHT_STYLE);

    frame.render_stateful_widget(table, area, &mut state.agent_table_state);

    if !state.agents.is_empty() {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        let mut scrollbar_state = ScrollbarState::new(state.agents.len())
            .position(state.agent_table_state.selected().unwrap_or(0));
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn render_skill_table(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let header = Row::new(vec!["Name", "Status", "Versions", "Tags", "Uploaded By"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = state
        .skills
        .iter()
        .map(|s| {
            let tags = if s.tags.is_empty() {
                "—".to_string()
            } else {
                s.tags.join(", ")
            };
            Row::new(vec![
                s.name.clone(),
                s.status.clone(),
                s.versions.to_string(),
                tags,
                s.uploaded_by.clone(),
            ])
        })
        .collect();

    let name_col_width = state
        .skills
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4) as u16
        + 1;

    let widths = [
        Constraint::Length(name_col_width),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Percentage(40),
        Constraint::Percentage(100),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(format!(" Skill Registry  ({}) ", state.skills.len())),
        )
        .row_highlight_style(HIGHLIGHT_STYLE);

    frame.render_stateful_widget(table, area, &mut state.skill_table_state);

    if !state.skills.is_empty() {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        let mut scrollbar_state = ScrollbarState::new(state.skills.len())
            .position(state.skill_table_state.selected().unwrap_or(0));
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn render_logs(frame: &mut Frame, state: &TuiState, area: Rect) {
    // Iterate in reverse (newest first) without an intermediate allocation.
    let lines: Vec<Line> = state
        .log_lines
        .iter()
        .rev()
        .map(|l| {
            let style = if l.contains("ERROR") || l.contains("error") {
                Style::default().fg(Color::Red)
            } else if l.contains("WARN") || l.contains("warn") {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(Span::styled(l.as_str(), style))
        })
        .collect();

    let total = lines.len();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style_for(state, FocusedPane::Logs))
        .title(format!(" Logs ({}) ", total));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((state.log_scroll as u16, 0));
    frame.render_widget(paragraph, area);

    if total + 2 > area.height as usize {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        let mut scrollbar_state = ScrollbarState::new(total).position(state.log_scroll);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

/// Renders a contextual hint bar showing keyboard shortcuts for the focused pane.
fn render_hint_bar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::default().fg(Color::DarkGray);
    let desc_style = Style::default().add_modifier(Modifier::DIM);

    let sep = Span::styled(" │ ", sep_style);

    let pane_label = match state.focused_pane {
        FocusedPane::TopLeft => "Server Info",
        FocusedPane::TopRight => "Endpoints & TLS",
        FocusedPane::Prime => "Agent Primer",
        FocusedPane::Tables => match state.tab_index {
            0 => "Chains",
            1 => "Agents",
            2 => "Skills",
            _ => "Tables",
        },
        FocusedPane::Logs => "Logs",
    };

    let mut spans = vec![
        Span::styled(
            format!(" {pane_label} "),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        sep.clone(),
    ];

    // Context-specific hints.
    match state.focused_pane {
        FocusedPane::TopLeft | FocusedPane::TopRight => {
            spans.extend_from_slice(&[
                Span::styled("↑↓", key_style),
                Span::styled(" scroll  ", desc_style),
            ]);
        }
        FocusedPane::Logs => {
            spans.extend_from_slice(&[
                Span::styled("↑↓", key_style),
                Span::styled(" scroll  ", desc_style),
                sep.clone(),
                Span::styled("c", key_style),
                Span::styled(" copy visible logs  ", desc_style),
            ]);
        }
        FocusedPane::Prime => {
            spans.extend_from_slice(&[
                Span::styled("c", key_style),
                Span::styled(" copy primer text  ", desc_style),
            ]);
        }
        FocusedPane::Tables => {
            spans.extend_from_slice(&[
                Span::styled("↑↓", key_style),
                Span::styled(" select row  ", desc_style),
                sep.clone(),
                Span::styled("←→", key_style),
                Span::styled(" switch tab  ", desc_style),
                sep.clone(),
                Span::styled("c", key_style),
                Span::styled(" copy key  ", desc_style),
            ]);
        }
    }

    // Global hints.
    spans.extend_from_slice(&[
        sep.clone(),
        Span::styled("Tab", key_style),
        Span::styled(" next pane  ", desc_style),
        sep.clone(),
        Span::styled("Click", key_style),
        Span::styled(" focus  ", desc_style),
        sep.clone(),
        Span::styled("Scroll", key_style),
        Span::styled(" mouse wheel  ", desc_style),
        sep,
        Span::styled("q", key_style),
        Span::styled(" quit", desc_style),
    ]);

    let hint_line = Line::from(spans);
    let paragraph =
        Paragraph::new(hint_line).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(paragraph, area);
}

fn render_startup_overlay(frame: &mut Frame, state: &TuiState, full_area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            &state.startup_status,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press q to quit during startup.",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Startup ");

    let box_width = 50u16.min(full_area.width);
    let box_height = lines.len() as u16 + 4;
    let popup = Rect {
        x: full_area.width.saturating_sub(box_width) / 2,
        y: full_area.height.saturating_sub(box_height) / 2,
        width: box_width,
        height: box_height,
    };

    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let inner_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(lines.len() as u16)])
        .split(inner);

    let paragraph = Paragraph::new(lines)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner_layout[0]);
}

fn render_update_dialog_overlay(frame: &mut Frame, state: &TuiState, full_area: Rect) {
    let Some((ref current, ref latest, ref url)) = state.update_dialog else {
        return;
    };

    let lines = vec![
        Line::from(Span::styled(
            "mentisdbd update available",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("Current core version: {current}")),
        Line::from(format!("Latest release tag : {latest}")),
        Line::from(format!("Release page       : {url}")),
        Line::from(""),
        Line::from(Span::styled(
            "Install release and restart now? [y/N]",
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Update ");

    let box_width = 60u16.min(full_area.width);
    let box_height = (lines.len() + 4) as u16;
    let popup = Rect {
        x: full_area.width.saturating_sub(box_width) / 2,
        y: full_area.height.saturating_sub(box_height) / 2,
        width: box_width,
        height: box_height,
    };

    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let inner_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(lines.len() as u16)])
        .split(inner);

    let paragraph = Paragraph::new(lines)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner_layout[0]);
}

fn render_toast(frame: &mut Frame, msg: &str, anchor: Rect) {
    let width = (msg.len() as u16 + 4).min(anchor.width.max(24)).max(24);
    let height = 3u16;
    let x = anchor.x + anchor.width.saturating_sub(width) / 2;
    let y = anchor.y + 1;
    let full = frame.area();
    let area = Rect {
        x: x.min(full.width.saturating_sub(width)),
        y: y.min(full.height.saturating_sub(height)),
        width,
        height,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let paragraph = Paragraph::new(Span::styled(
        msg,
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    ))
    .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(paragraph, inner);
}

fn render_crash_overlay(frame: &mut Frame, err: &str, full_area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            "mentisdbd startup failed",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(err, Style::default().fg(Color::Red))),
        Line::from(""),
        Line::from(Span::styled(
            "Check the Logs pane for details.  Press q to quit.",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Red))
        .title(Span::styled(
            " Startup Error ",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));

    let box_width = 72u16.min(full_area.width);
    let box_height = (lines.len() as u16 + 4).min(full_area.height);
    let popup = Rect {
        x: full_area.width.saturating_sub(box_width) / 2,
        y: full_area.height.saturating_sub(box_height) / 2,
        width: box_width,
        height: box_height,
    };

    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let paragraph = Paragraph::new(lines)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

///
/// This ensures the terminal is cleaned up even when an error propagates
/// through `?` before reaching explicit cleanup code.
struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            io::stdout(),
            DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}

pub fn run_tui(
    state: Arc<std::sync::Mutex<TuiState>>,
    running: Arc<AtomicBool>,
    log_rx: mpsc::Receiver<String>,
) -> io::Result<()> {
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    crossterm::execute!(stdout, EnableMouseCapture)?;
    crossterm::terminal::enable_raw_mode()?;

    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut clipboard = arboard::Clipboard::new().ok();

    loop {
        if !running.load(Ordering::SeqCst) {
            let mut s = state.lock().unwrap();
            s.should_quit = true;
        }

        // Drain pending log messages from the custom logger channel.
        while let Ok(line) = log_rx.try_recv() {
            let mut s = state.lock().unwrap();
            s.add_log(line);
        }

        // Expire the toast after 2 seconds.
        {
            let mut s = state.lock().unwrap();
            if let Some((_, instant)) = s.toast {
                if instant.elapsed() > Duration::from_secs(2) {
                    s.toast = None;
                }
            }
        }

        terminal.draw(|frame| {
            let mut s = state.lock().unwrap();
            ui(frame, &mut s);
        })?;

        if event::poll(Duration::from_millis(100))? {
            // text_to_copy is extracted while holding the lock; the actual
            // clipboard write and toast update happen outside the lock.
            let text_to_copy: Option<String> = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let mut s = state.lock().unwrap();
                    // When update dialog is shown, only y/n/Esc/Enter matter.
                    if s.update_dialog.is_some() {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                if let Some(tx) = s.update_response_tx.take() {
                                    let _ = tx.send(true);
                                }
                                s.update_dialog = None;
                            }
                            KeyCode::Char('n')
                            | KeyCode::Char('N')
                            | KeyCode::Enter
                            | KeyCode::Esc => {
                                if let Some(tx) = s.update_response_tx.take() {
                                    let _ = tx.send(false);
                                }
                                s.update_dialog = None;
                            }
                            _ => {}
                        }
                        None
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                s.should_quit = true;
                                None
                            }
                            // 'c' copies the focused item to clipboard.
                            KeyCode::Char('c') | KeyCode::Char('C') => {
                                s.selected_item_text()
                            }
                            // Tab cycles pane focus; Shift+Tab reverses.
                            KeyCode::Tab => {
                                s.focused_pane = s.focused_pane.next();
                                None
                            }
                            KeyCode::BackTab => {
                                s.focused_pane = s.focused_pane.prev();
                                None
                            }
                            // Arrow keys route based on focused pane.
                            KeyCode::Up => {
                                match s.focused_pane {
                                    FocusedPane::TopLeft => s.scroll_left_up(),
                                    FocusedPane::TopRight => s.scroll_right_up(),
                                    FocusedPane::Logs => s.scroll_logs_up(),
                                    FocusedPane::Prime => {}
                                    FocusedPane::Tables => match s.tab_index {
                                        0 => s.select_chain_prev(),
                                        1 => s.select_agent_prev(),
                                        2 => s.select_skill_prev(),
                                        _ => {}
                                    },
                                }
                                None
                            }
                            KeyCode::Down => {
                                match s.focused_pane {
                                    FocusedPane::TopLeft => s.scroll_left_down(),
                                    FocusedPane::TopRight => s.scroll_right_down(),
                                    FocusedPane::Logs => s.scroll_logs_down(),
                                    FocusedPane::Prime => {}
                                    FocusedPane::Tables => match s.tab_index {
                                        0 => s.select_chain_next(),
                                        1 => s.select_agent_next(),
                                        2 => s.select_skill_next(),
                                        _ => {}
                                    },
                                }
                                None
                            }
                            // Left/Right switch table tabs when Tables focused.
                            KeyCode::Left if s.focused_pane == FocusedPane::Tables => {
                                let len = s.tab_titles.len();
                                s.tab_index = if s.tab_index == 0 {
                                    len - 1
                                } else {
                                    s.tab_index - 1
                                };
                                None
                            }
                            KeyCode::Right if s.focused_pane == FocusedPane::Tables => {
                                s.tab_index = (s.tab_index + 1) % s.tab_titles.len();
                                None
                            }
                            KeyCode::PageUp => {
                                s.scroll_right_up();
                                None
                            }
                            KeyCode::PageDown => {
                                s.scroll_right_down();
                                None
                            }
                            _ => None,
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    let pos = Position {
                        x: mouse.column,
                        y: mouse.row,
                    };
                    // MouseUp: extract text from the last rendered buffer.
                    // Must happen OUTSIDE the state lock so we can borrow
                    // `terminal` at the same time.
                    if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
                        let maybe_start = {
                            let mut s = state.lock().unwrap();
                            let start = s.drag_start.take();
                            s.drag_current = None;
                            start
                        };
                        if let Some(start) = maybe_start {
                            let moved =
                                start.y != pos.y || pos.x.abs_diff(start.x) > 3;
                            if moved {
                                extract_from_buffer(terminal.current_buffer_mut(), start, pos)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        let mut s = state.lock().unwrap();
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                s.drag_start = Some(pos);
                                s.drag_current = Some(pos);
                                if s.last_top_left_area.contains(pos) {
                                    s.focused_pane = FocusedPane::TopLeft;
                                } else if s.last_top_right_area.contains(pos) {
                                    s.focused_pane = FocusedPane::TopRight;
                                } else if s.last_prime_area.contains(pos) {
                                    s.focused_pane = FocusedPane::Prime;
                                } else if s.last_logs_area.contains(pos) {
                                    s.focused_pane = FocusedPane::Logs;
                                } else if s.last_tables_area.contains(pos)
                                    || s.last_tabs_area.contains(pos)
                                {
                                    s.focused_pane = FocusedPane::Tables;
                                    if s.last_tabs_area.contains(pos) {
                                        if let Some(idx) = tab_index_from_click(
                                            &s.tab_titles,
                                            s.last_tabs_area,
                                            pos.x,
                                        ) {
                                            s.tab_index = idx;
                                        }
                                    }
                                }
                                None
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                s.drag_current = Some(pos);
                                None
                            }
                            MouseEventKind::ScrollUp => {
                                if s.last_tables_area.contains(pos) {
                                    match s.tab_index {
                                        0 => s.select_chain_prev(),
                                        1 => s.select_agent_prev(),
                                        2 => s.select_skill_prev(),
                                        _ => {}
                                    }
                                } else if s.last_logs_area.contains(pos) {
                                    s.scroll_logs_up();
                                } else if s.last_top_right_area.contains(pos) {
                                    s.scroll_right_up();
                                } else if s.last_top_left_area.contains(pos) {
                                    s.scroll_left_up();
                                }
                                None
                            }
                            MouseEventKind::ScrollDown => {
                                if s.last_tables_area.contains(pos) {
                                    match s.tab_index {
                                        0 => s.select_chain_next(),
                                        1 => s.select_agent_next(),
                                        2 => s.select_skill_next(),
                                        _ => {}
                                    }
                                } else if s.last_logs_area.contains(pos) {
                                    s.scroll_logs_down();
                                } else if s.last_top_right_area.contains(pos) {
                                    s.scroll_right_down();
                                } else if s.last_top_left_area.contains(pos) {
                                    s.scroll_left_down();
                                }
                                None
                            }
                            _ => None,
                        }
                    }
                }
                _ => None,
            };

            // Clipboard write and toast happen outside the state lock.
            if let Some(ref text) = text_to_copy {
                let copied = clipboard
                    .as_mut()
                    .map(|cb| cb.set_text(text.as_str()).is_ok())
                    .unwrap_or(false);
                if copied {
                    let mut s = state.lock().unwrap();
                    s.toast = Some(("Text copied to clipboard!".to_string(), Instant::now()));
                }
            }
        }

        {
            let s = state.lock().unwrap();
            if s.should_quit {
                break;
            }
        }
    }

    Ok(())
}

/// Reads the rendered text in the drag rectangle directly from ratatui's
/// buffer. This works for any content on screen — logs, tables, config,
/// primer text — without needing per-pane extraction logic.
fn extract_from_buffer(buffer: &Buffer, start: Position, end: Position) -> Option<String> {
    let y1 = start.y.min(end.y);
    let y2 = start.y.max(end.y);
    let x1 = start.x.min(end.x);
    let x2 = start.x.max(end.x);

    let mut lines: Vec<String> = Vec::new();
    for y in y1..=y2 {
        let mut line = String::new();
        for x in x1..=x2 {
            if let Some(cell) = buffer.cell(Position { x, y }) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_string());
    }
    // Drop trailing blank lines.
    while lines.last().map(|l: &String| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Determines which tab index was clicked based on x position within the tab bar.
fn tab_index_from_click(titles: &[&str], tabs_area: Rect, click_x: u16) -> Option<usize> {
    let mut x = tabs_area.x + 1; // 1-char left border
    for (i, title) in titles.iter().enumerate() {
        let tab_width = title.len() as u16 + 2; // " {title} "
        if click_x >= x && click_x < x + tab_width {
            return Some(i);
        }
        x += tab_width + 1; // +1 for "|" divider
    }
    None
}

