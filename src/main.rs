use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use rusqlite::Connection;

const TICK_RATE: Duration = Duration::from_secs(1);
const STALE_THRESHOLD_SECS: i64 = 300;

struct Session {
    session_id: String,
    status: String,
    tool_name: Option<String>,
    detail: Option<String>,
    cwd: Option<String>,
    tmux_session: Option<String>,
    updated_at: String,
}

fn project_name(cwd: &Option<String>) -> String {
    cwd.as_deref()
        .and_then(|p| std::path::Path::new(p).file_name().and_then(|f| f.to_str()))
        .unwrap_or("?")
        .to_string()
}

fn status_emoji(status: &str, is_stale: bool) -> &'static str {
    if is_stale {
        return "💤";
    }
    match status {
        "working" => "🔨",
        "waiting" => "🔔",
        "idle" => "⏸️ ",
        _ => "❓",
    }
}

fn status_color(status: &str, is_stale: bool) -> Color {
    if is_stale {
        return Color::Cyan;
    }
    match status {
        "working" => Color::Green,
        "waiting" => Color::Yellow,
        "idle" => Color::Gray,
        _ => Color::Gray,
    }
}

fn parse_timestamp(updated_at: &str) -> i64 {
    let parts: Vec<&str> = updated_at
        .split(|c| c == '-' || c == ' ' || c == ':')
        .collect();
    if parts.len() < 6 {
        return 0;
    }
    let (year, month, day, hour, min, sec) = match (
        parts[0].parse::<i64>(),
        parts[1].parse::<i64>(),
        parts[2].parse::<i64>(),
        parts[3].parse::<i64>(),
        parts[4].parse::<i64>(),
        parts[5].parse::<i64>(),
    ) {
        (Ok(y), Ok(mo), Ok(d), Ok(h), Ok(mi), Ok(s)) => (y, mo, d, h, mi, s),
        _ => return 0,
    };

    let days_in_month: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut total_days: i64 = 0;
    for y in 1970..year {
        total_days += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
    }
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    for m in 0..(month - 1) as usize {
        total_days += days_in_month[m];
        if m == 1 && is_leap {
            total_days += 1;
        }
    }
    total_days += day - 1;
    total_days * 86400 + hour * 3600 + min * 60 + sec
}

fn seconds_since(updated_at: &str) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    now - parse_timestamp(updated_at)
}

fn relative_time(updated_at: &str) -> String {
    let diff = seconds_since(updated_at);
    if diff < 0 {
        return "just now".to_string();
    }
    if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

fn db_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".claude")
        .join("claude-jam.db")
}

fn open_db() -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=3000;
         CREATE TABLE IF NOT EXISTS sessions (
             session_id TEXT PRIMARY KEY,
             status TEXT NOT NULL,
             tool_name TEXT,
             detail TEXT,
             cwd TEXT,
             tmux_session TEXT,
             started_at DATETIME,
             updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );",
    )?;
    Ok(conn)
}

fn fetch_sessions(conn: &Connection) -> Vec<Session> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, status, tool_name, detail, cwd, tmux_session, updated_at
             FROM sessions
             WHERE status != 'offline'
             ORDER BY updated_at DESC",
        )
        .unwrap();

    stmt.query_map([], |row| {
        Ok(Session {
            session_id: row.get(0)?,
            status: row.get(1)?,
            tool_name: row.get(2)?,
            detail: row.get(3)?,
            cwd: row.get(4)?,
            tmux_session: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn delete_session(conn: &Connection, session_id: &str) {
    let _ = conn.execute("DELETE FROM sessions WHERE session_id = ?1", [session_id]);
}

fn switch_tmux_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["switch-client", "-t", session_name])
        .status();
}

struct App {
    sessions: Vec<Session>,
    selected: usize,
}

impl App {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected: 0,
        }
    }

    fn refresh(&mut self, conn: &Connection) {
        self.sessions = fetch_sessions(conn);
        if self.selected >= self.sessions.len() && !self.sessions.is_empty() {
            self.selected = self.sessions.len() - 1;
        }
    }

    fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.selected)
    }

    fn session_at_shortcut(&self, c: char) -> Option<usize> {
        let idx = match c {
            '1'..='9' => (c as usize) - ('1' as usize),
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

fn shortcut_label(i: usize) -> String {
    match i {
        0..=8 => format!("{}", i + 1),
        9..=34 => format!("{}", (b'a' + (i - 9) as u8) as char),
        _ => " ".to_string(),
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area());

    let area = chunks[0];
    let width = area.width.saturating_sub(4) as usize; // border + padding

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let is_stale = seconds_since(&s.updated_at) > STALE_THRESHOLD_SECS;
            let emoji = status_emoji(&s.status, is_stale);
            let color = status_color(&s.status, is_stale);
            let project = project_name(&s.cwd);
            let time = relative_time(&s.updated_at);
            let tmux = s
                .tmux_session
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(|t| format!(" [{}]", t))
                .unwrap_or_default();

            // Line 1: shortcut + emoji + project name + tmux session + time
            let label = shortcut_label(i);
            let header_line = Line::from(vec![
                Span::styled(format!("{} ", label), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{} ", emoji), Style::default()),
                Span::styled(
                    format!("{}{}", project, tmux),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", time), Style::default().fg(Color::Gray)),
            ]);

            // Line 2: tool + detail (show "Done" for idle sessions)
            let tool = if s.status == "idle" {
                ""
            } else {
                s.tool_name.as_deref().unwrap_or("")
            };
            let detail = if s.status == "idle" {
                "Done"
            } else {
                s.detail.as_deref().unwrap_or("")
            };
            let max_detail = width.saturating_sub(tool.len() + 6); // "  ├ " + tool + " "
            let detail_truncated = if detail.len() > max_detail {
                format!("{}…", &detail[..max_detail.saturating_sub(1)])
            } else {
                detail.to_string()
            };

            let detail_line = Line::from(vec![
                Span::styled("  ├ ", Style::default().fg(Color::Gray)),
                Span::styled(tool.to_string(), Style::default().fg(Color::LightBlue)),
                Span::styled(" ", Style::default()),
                Span::styled(detail_truncated, Style::default().fg(Color::Gray)),
            ]);

            if i == app.selected {
                ListItem::new(vec![header_line, detail_line])
                    .style(Style::default().bg(Color::DarkGray).fg(Color::White))
            } else {
                ListItem::new(vec![header_line, detail_line])
            }
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Claude Jam ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    frame.render_widget(list, area);

    if app.sessions.is_empty() {
        let msg = Paragraph::new("No active sessions")
            .style(Style::default().fg(Color::DarkGray))
            .centered();
        let inner = area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 2,
        });
        frame.render_widget(msg, inner);
    }

    // Footer
    let footer = Line::from(vec![
        Span::raw(" 1-9/a-z").bold(),
        Span::raw(" jump  "),
        Span::raw("↑↓").bold(),
        Span::raw(" navigate  "),
        Span::raw("Enter").bold(),
        Span::raw(" switch  "),
        Span::raw("C-d").bold(),
        Span::raw(" delete  "),
        Span::raw("C-q").bold(),
        Span::raw(" quit  "),
    ])
    .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(Paragraph::new(footer), chunks[1]);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let quit_on_select = std::env::args().any(|a| a == "-q" || a == "--quit");
    let conn = open_db()?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.refresh(&conn);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui(f, &app))?;

        let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Helper closure for tmux switching
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
                        KeyCode::Char('q') if ctrl => break,
                        KeyCode::Esc => break,
                        KeyCode::Down => {
                            if !app.sessions.is_empty() {
                                app.selected = (app.selected + 1) % app.sessions.len();
                            }
                        }
                        KeyCode::Char('j') if ctrl => {
                            if !app.sessions.is_empty() {
                                app.selected = (app.selected + 1) % app.sessions.len();
                            }
                        }
                        KeyCode::Up => {
                            if !app.sessions.is_empty() {
                                app.selected = if app.selected == 0 {
                                    app.sessions.len() - 1
                                } else {
                                    app.selected - 1
                                };
                            }
                        }
                        KeyCode::Char('k') if ctrl => {
                            if !app.sessions.is_empty() {
                                app.selected = if app.selected == 0 {
                                    app.sessions.len() - 1
                                } else {
                                    app.selected - 1
                                };
                            }
                        }
                        KeyCode::Enter => {
                            switch(app.selected, &mut terminal)?;
                            if quit_on_select {
                                break;
                            }
                            terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
                        }
                        KeyCode::Char('d') if ctrl => {
                            if let Some(session) = app.selected_session() {
                                let id = session.session_id.clone();
                                delete_session(&conn, &id);
                                app.refresh(&conn);
                            }
                        }
                        KeyCode::Char(c) if !ctrl => {
                            if let Some(idx) = app.session_at_shortcut(c) {
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
            app.refresh(&conn);
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
