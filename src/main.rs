use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::{Frame, Terminal};
use rusqlite::Connection;

const TICK_RATE: Duration = Duration::from_secs(1);
const STALE_THRESHOLD_SECS: i64 = 300; // 5 minutes

struct Session {
    session_id: String,
    status: String,
    tool_name: Option<String>,
    detail: Option<String>,
    cwd: Option<String>,
    updated_at: String,
}


fn project_name(cwd: &Option<String>) -> String {
    cwd.as_deref()
        .and_then(|p| {
            std::path::Path::new(p)
                .file_name()
                .and_then(|f| f.to_str())
        })
        .unwrap_or("?")
        .to_string()
}

fn status_color(status: &str) -> Color {
    match status {
        "working" => Color::Green,
        "waiting" => Color::Yellow,
        "idle" => Color::DarkGray,
        _ => Color::DarkGray,
    }
}

fn relative_time(updated_at: &str) -> String {
    // updated_at is in SQLite CURRENT_TIMESTAMP format: "YYYY-MM-DD HH:MM:SS"
    // We'll parse it and compare to now (UTC)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Parse the SQLite timestamp manually
    let parts: Vec<&str> = updated_at.split(|c| c == '-' || c == ' ' || c == ':').collect();
    if parts.len() < 6 {
        return updated_at.to_string();
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
        _ => return updated_at.to_string(),
    };

    // Approximate unix timestamp (no leap seconds, but good enough)
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
    let ts = total_days * 86400 + hour * 3600 + min * 60 + sec;

    let diff = now as i64 - ts;
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

fn seconds_since(updated_at: &str) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let parts: Vec<&str> = updated_at.split(|c| c == '-' || c == ' ' || c == ':').collect();
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
    let ts = total_days * 86400 + hour * 3600 + min * 60 + sec;
    now - ts
}

fn db_path() -> PathBuf {
    dirs_or_home().join(".claude").join("claude-jam.db")
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn open_db() -> rusqlite::Result<Connection> {
    let path = db_path();
    let conn = Connection::open(&path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=3000;
         CREATE TABLE IF NOT EXISTS sessions (
             session_id TEXT PRIMARY KEY,
             status TEXT NOT NULL,
             tool_name TEXT,
             detail TEXT,
             cwd TEXT,
             started_at DATETIME,
             updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );",
    )?;
    Ok(conn)
}

fn fetch_sessions(conn: &Connection) -> Vec<Session> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, status, tool_name, detail, cwd, updated_at
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
            updated_at: row.get(5)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn delete_session(conn: &Connection, session_id: &str) {
    let _ = conn.execute("DELETE FROM sessions WHERE session_id = ?1", [session_id]);
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

    fn selected_session_id(&self) -> Option<&str> {
        self.sessions.get(self.selected).map(|s| s.session_id.as_str())
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area());

    render_table(frame, app, chunks[0]);
    render_footer(frame, chunks[1]);
}

fn render_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(vec![
        Cell::from("Project").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Status").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Tool").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Detail").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Last Update").style(Style::default().add_modifier(Modifier::BOLD)),
    ])
    .height(1);

    let detail_width = area.width.saturating_sub(12 + 10 + 18 + 12 + 8) as usize;

    let rows: Vec<Row> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let is_stale = seconds_since(&s.updated_at) > STALE_THRESHOLD_SECS;
            let style = if is_stale {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };

            let status_display = if is_stale {
                "stale".to_string()
            } else {
                s.status.clone()
            };

            let detail_str = s.detail.as_deref().unwrap_or("");
            let detail_truncated: String = if detail_str.len() > detail_width {
                format!("{}...", &detail_str[..detail_width.saturating_sub(3)])
            } else {
                detail_str.to_string()
            };

            let row_style = if i == app.selected {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };

            Row::new(vec![
                Cell::from(project_name(&s.cwd)),
                Cell::from(status_display).style(Style::default().fg(status_color(&s.status))),
                Cell::from(s.tool_name.as_deref().unwrap_or("")),
                Cell::from(detail_truncated),
                Cell::from(relative_time(&s.updated_at)),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Fill(1),
        Constraint::Length(12),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(" Claude Jam ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );

    frame.render_widget(table, area);

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
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let footer = Line::from(vec![
        Span::raw(" q").bold(),
        Span::raw(" quit  "),
        Span::raw("j/k").bold(),
        Span::raw(" navigate  "),
        Span::raw("d").bold(),
        Span::raw(" delete stale  "),
    ])
    .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(Paragraph::new(footer), area);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('j') | KeyCode::Down => {
                            if !app.sessions.is_empty() {
                                app.selected = (app.selected + 1) % app.sessions.len();
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if !app.sessions.is_empty() {
                                app.selected = if app.selected == 0 {
                                    app.sessions.len() - 1
                                } else {
                                    app.selected - 1
                                };
                            }
                        }
                        KeyCode::Char('d') => {
                            if let Some(id) = app.selected_session_id() {
                                let id = id.to_string();
                                delete_session(&conn, &id);
                                app.refresh(&conn);
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
