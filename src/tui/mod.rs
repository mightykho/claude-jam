//! The TUI dashboard — application state, render glue, and event loop.

mod render;
mod style;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use rusqlite::Connection;

use claude_jam::db::{delete_session, fetch_sessions};
use claude_jam::models::Session;
use claude_jam::tmux::{current_tmux_session, switch_tmux_session};

use render::ui;

/// Polling interval for the event loop / database refresh.
const TICK_RATE: Duration = Duration::from_secs(1);

/// In-memory state owned by `run_tui`. Read by the render pipeline, mutated by
/// the key handler.
pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    /// Index of the session whose milestone history is expanded (`o` toggles it).
    pub expanded: Option<usize>,
    /// Index of the session pending delete confirmation (`d` opens the popup).
    pub pending_delete: Option<usize>,
    /// Render without the surrounding title/border (`-b`).
    pub borderless: bool,
    /// Render each session as title-line + detail-line (`-v`).
    pub vertical: bool,
    /// tmux session name cj was launched from, if any. Used to mark the
    /// matching row in the dashboard with a "you are here" indicator.
    pub current_tmux: Option<String>,
}

impl App {
    pub fn new(borderless: bool, vertical: bool, current_tmux: Option<String>) -> Self {
        Self {
            sessions: Vec::new(),
            selected: 0,
            expanded: None,
            pending_delete: None,
            borderless,
            vertical,
            current_tmux,
        }
    }

    pub fn refresh(&mut self, conn: &Connection) {
        self.sessions = fetch_sessions(conn);
        if self.selected >= self.sessions.len() && !self.sessions.is_empty() {
            self.selected = self.sessions.len() - 1;
        }
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.selected)
    }

    pub fn toggle_expand(&mut self) {
        if self.expanded == Some(self.selected) {
            self.expanded = None;
        } else {
            self.expanded = Some(self.selected);
        }
    }

    fn session_at_number(&self, c: char) -> Option<usize> {
        let idx = match c {
            '1'..='9' => (c as usize) - ('1' as usize),
            _ => return None,
        };
        if idx < self.sessions.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn session_at_ctrl_letter(&self, c: char) -> Option<usize> {
        let idx = match c {
            'a'..='z' => 9 + (c as usize) - ('a' as usize),
            _ => return None,
        };
        if idx < self.sessions.len() {
            Some(idx)
        } else {
            None
        }
    }
}

/// Run the TUI event loop.
///
/// Installs a panic hook that restores the terminal before propagating, so a
/// crash leaves the user's terminal usable instead of a wrecked one.
pub fn run_tui(
    conn: &Connection,
    quit_on_select: bool,
    borderless: bool,
    vertical: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let current_tmux = current_tmux_session();
    let mut app = App::new(borderless, vertical, current_tmux);
    app.refresh(conn);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui(f, &app, conn))?;

        let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if app.pending_delete.is_some() {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                if let Some(idx) = app.pending_delete {
                                    if let Some(session) = app.sessions.get(idx) {
                                        let id = session.session_id.clone();
                                        delete_session(conn, &id);
                                        app.refresh(conn);
                                    }
                                }
                                app.pending_delete = None;
                            }
                            KeyCode::Char('n')
                            | KeyCode::Char('N')
                            | KeyCode::Esc
                            | KeyCode::Char('q') => {
                                app.pending_delete = None;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    let switch = |idx: usize,
                                  terminal: &mut Terminal<CrosstermBackend<io::Stdout>>|
                     -> Result<(), Box<dyn std::error::Error>> {
                        if let Some(session) = app.sessions.get(idx) {
                            if let Some(ref tmux) = session.tmux_session {
                                if !tmux.is_empty() {
                                    disable_raw_mode()?;
                                    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                                    terminal.show_cursor()?;
                                    switch_tmux_session(tmux);
                                    enable_raw_mode()?;
                                    execute!(io::stdout(), EnterAlternateScreen)?;
                                }
                            }
                        }
                        Ok(())
                    };

                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Char('q') if !ctrl => break,
                        KeyCode::Esc => break,
                        KeyCode::Char('j') | KeyCode::Down if !ctrl && !app.sessions.is_empty() => {
                            app.selected = (app.selected + 1) % app.sessions.len();
                        }
                        KeyCode::Char('k') | KeyCode::Up if !ctrl && !app.sessions.is_empty() => {
                            app.selected = if app.selected == 0 {
                                app.sessions.len() - 1
                            } else {
                                app.selected - 1
                            };
                        }
                        KeyCode::Char('o') if !ctrl => {
                            app.toggle_expand();
                        }
                        KeyCode::Enter => {
                            switch(app.selected, &mut terminal)?;
                            if quit_on_select {
                                break;
                            }
                            terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
                        }
                        KeyCode::Char('d') if !ctrl && app.selected_session().is_some() => {
                            app.pending_delete = Some(app.selected);
                        }
                        KeyCode::Char(c @ '1'..='9') if !ctrl => {
                            if let Some(idx) = app.session_at_number(c) {
                                app.selected = idx;
                                switch(idx, &mut terminal)?;
                                if quit_on_select {
                                    break;
                                }
                                terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
                            }
                        }
                        KeyCode::Char(c) if ctrl => {
                            if let Some(idx) = app.session_at_ctrl_letter(c) {
                                app.selected = idx;
                                switch(idx, &mut terminal)?;
                                if quit_on_select {
                                    break;
                                }
                                terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.refresh(conn);
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
