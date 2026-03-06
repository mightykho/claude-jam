use std::io::{self, Read as _};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

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
    topic: Option<String>,
    updated_at: String,
}

struct Milestone {
    summary: String,
    created_at: String,
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
        "idle" => "✅",
        "pending" => "⏳",
        _ => "❓",
    }
}

fn status_color(status: &str, is_stale: bool) -> Color {
    if is_stale {
        return Color::Cyan;
    }
    match status {
        "working" => Color::Blue,
        "waiting" => Color::Yellow,
        "idle" => Color::Green,
        "pending" => Color::DarkGray,
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
             topic TEXT,
             started_at DATETIME,
             updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS milestones (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL,
             summary TEXT NOT NULL,
             created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
             FOREIGN KEY (session_id) REFERENCES sessions(session_id)
         );",
    )?;
    // Migrations for existing DBs
    let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN topic TEXT;");
    Ok(conn)
}

fn current_tmux_session() -> Option<String> {
    Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        })
}

fn find_session_by_tmux(conn: &Connection, tmux_name: &str) -> Option<String> {
    conn.query_row(
        "SELECT session_id FROM sessions WHERE tmux_session = ?1 AND status != 'offline' ORDER BY updated_at DESC LIMIT 1",
        [tmux_name],
        |row| row.get(0),
    )
    .ok()
}

fn cmd_topic(conn: &Connection, text: &str) {
    let tmux = match current_tmux_session() {
        Some(t) => t,
        None => {
            eprintln!("Error: not in a tmux session");
            std::process::exit(1);
        }
    };
    let session_id = match find_session_by_tmux(&conn, &tmux) {
        Some(id) => id,
        None => {
            eprintln!("Error: no active session found for tmux session '{}'", tmux);
            std::process::exit(1);
        }
    };
    conn.execute(
        "UPDATE sessions SET topic = ?1 WHERE session_id = ?2",
        rusqlite::params![text, session_id],
    )
    .unwrap();
    println!("Topic set for session in '{}'", tmux);
}

fn cmd_init(conn: &Connection, topic: &str) {
    let tmux = match current_tmux_session() {
        Some(t) => t,
        None => {
            eprintln!("Error: not in a tmux session");
            std::process::exit(1);
        }
    };
    let placeholder_id = format!("tmux:{}", tmux);
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, topic, started_at, updated_at)
         VALUES (?1, 'pending', ?2, ?3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(session_id) DO UPDATE SET topic=excluded.topic, updated_at=CURRENT_TIMESTAMP",
        rusqlite::params![placeholder_id, tmux, topic],
    )
    .unwrap();
    println!("Session initialized in '{}' with topic: {}", tmux, topic);
}

#[derive(Deserialize, Default)]
struct HookInput {
    session_id: Option<String>,
    hook_event_name: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    cwd: Option<String>,
    prompt: Option<String>,
    message: Option<String>,
}

fn event_to_status(event: &str) -> &str {
    match event {
        "SessionStart" => "idle",
        "UserPromptSubmit" => "working",
        "PreToolUse" => "working",
        "PostToolUse" => "working",
        "PostToolUseFailure" => "working",
        "Notification" => "waiting",
        "Stop" => "idle",
        "SessionEnd" => "offline",
        _ => "working",
    }
}

fn extract_detail(input: &HookInput) -> String {
    if let Some(ref ti) = input.tool_input {
        if let Some(s) = ti.get("command").and_then(|v| v.as_str()) {
            return s.chars().take(200).collect();
        }
        if let Some(s) = ti.get("file_path").and_then(|v| v.as_str()) {
            return s.chars().take(200).collect();
        }
        if let Some(s) = ti.get("pattern").and_then(|v| v.as_str()) {
            return s.chars().take(200).collect();
        }
    }
    if let Some(ref s) = input.prompt {
        return s.chars().take(200).collect();
    }
    if let Some(ref s) = input.message {
        return s.chars().take(200).collect();
    }
    String::new()
}

fn cmd_hook(conn: &Connection) {
    let mut input_str = String::new();
    io::stdin().read_to_string(&mut input_str).unwrap_or(0);

    let input: HookInput = serde_json::from_str(&input_str).unwrap_or_default();

    let session_id = match input.session_id {
        Some(ref id) if !id.is_empty() => id.clone(),
        _ => return,
    };

    let event = input.hook_event_name.as_deref().unwrap_or("");
    let status = event_to_status(event);
    let tool = input.tool_name.as_deref().unwrap_or("");
    let detail = extract_detail(&input);
    let cwd = input.cwd.as_deref().unwrap_or("");

    let tmux_session = current_tmux_session().unwrap_or_default();

    // On SessionStart, adopt any placeholder from `cj init`
    if event == "SessionStart" && !tmux_session.is_empty() {
        let placeholder_id = format!("tmux:{}", tmux_session);
        let topic: Option<String> = conn
            .query_row(
                "SELECT topic FROM sessions WHERE session_id = ?1",
                [&placeholder_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        if let Some(ref topic_val) = topic {
            // Move milestones from placeholder to real session
            let _ = conn.execute(
                "UPDATE milestones SET session_id = ?1 WHERE session_id = ?2",
                rusqlite::params![session_id, placeholder_id],
            );
            // Delete placeholder
            let _ = conn.execute(
                "DELETE FROM sessions WHERE session_id = ?1",
                [&placeholder_id],
            );
            // Insert real session with adopted topic
            conn.execute(
                "INSERT INTO sessions (session_id, status, tool_name, detail, cwd, tmux_session, topic, started_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(session_id) DO UPDATE SET status=excluded.status, tool_name=excluded.tool_name, detail=excluded.detail, cwd=excluded.cwd, tmux_session=excluded.tmux_session, topic=excluded.topic, updated_at=CURRENT_TIMESTAMP",
                rusqlite::params![session_id, status, tool, detail, cwd, tmux_session, topic_val],
            )
            .unwrap();
            return;
        }
    }

    // Normal upsert — preserve existing topic
    conn.execute(
        "INSERT INTO sessions (session_id, status, tool_name, detail, cwd, tmux_session, started_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(session_id) DO UPDATE SET status=excluded.status, tool_name=excluded.tool_name, detail=excluded.detail, cwd=excluded.cwd, tmux_session=excluded.tmux_session, updated_at=CURRENT_TIMESTAMP",
        rusqlite::params![session_id, status, tool, detail, cwd, tmux_session],
    )
    .unwrap();
}

fn cmd_milestone(conn: &Connection, text: &str) {
    let tmux = match current_tmux_session() {
        Some(t) => t,
        None => {
            eprintln!("Error: not in a tmux session");
            std::process::exit(1);
        }
    };
    let session_id = match find_session_by_tmux(&conn, &tmux) {
        Some(id) => id,
        None => {
            eprintln!("Error: no active session found for tmux session '{}'", tmux);
            std::process::exit(1);
        }
    };
    conn.execute(
        "INSERT INTO milestones (session_id, summary) VALUES (?1, ?2)",
        rusqlite::params![session_id, text],
    )
    .unwrap();
    println!("Milestone added for session in '{}'", tmux);
}

fn fetch_sessions(conn: &Connection) -> Vec<Session> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, status, tool_name, detail, cwd, tmux_session, topic, updated_at
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
            topic: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn fetch_latest_milestone(conn: &Connection, session_id: &str) -> Option<Milestone> {
    conn.query_row(
        "SELECT summary, created_at FROM milestones WHERE session_id = ?1 ORDER BY created_at DESC LIMIT 1",
        [session_id],
        |row| {
            Ok(Milestone {
                summary: row.get(0)?,
                created_at: row.get(1)?,
            })
        },
    )
    .ok()
}

fn fetch_milestones(conn: &Connection, session_id: &str) -> Vec<Milestone> {
    let mut stmt = conn
        .prepare("SELECT summary, created_at FROM milestones WHERE session_id = ?1 ORDER BY created_at DESC")
        .unwrap();

    stmt.query_map([session_id], |row| {
        Ok(Milestone {
            summary: row.get(0)?,
            created_at: row.get(1)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn delete_session(conn: &Connection, session_id: &str) {
    let _ = conn.execute("DELETE FROM milestones WHERE session_id = ?1", [session_id]);
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
    expanded: Option<usize>, // index of session with milestones expanded
}

impl App {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected: 0,
            expanded: None,
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

    fn toggle_expand(&mut self) {
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

fn shortcut_label(i: usize) -> String {
    match i {
        0..=8 => format!("{}", i + 1),
        9..=34 => format!("{}", (b'a' + (i - 9) as u8) as char),
        _ => " ".to_string(),
    }
}

fn ui(frame: &mut Frame, app: &App, conn: &Connection) {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area());

    let area = chunks[0];
    let width = area.width.saturating_sub(4) as usize;

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

            let label = shortcut_label(i);
            let mut lines = vec![];

            // Line 1: shortcut + emoji + project name + tmux session + time
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", label), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{} ", emoji), Style::default()),
                Span::styled(
                    format!("{}{}", project, tmux),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", time), Style::default().fg(Color::Gray)),
            ]));

            // Line 2: topic or tool+detail
            let latest_milestone = fetch_latest_milestone(conn, &s.session_id);

            if let Some(ref topic) = s.topic {
                let max_topic = width.saturating_sub(4);
                let topic_display = if topic.len() > max_topic {
                    format!("{}…", &topic[..max_topic.saturating_sub(1)])
                } else {
                    topic.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(Color::Gray)),
                    Span::styled(topic_display, Style::default().fg(Color::White)),
                ]));
            }

            // Line 3: latest milestone or tool+detail
            if let Some(ref ms) = latest_milestone {
                let ms_time = relative_time(&ms.created_at);
                let prefix = if s.topic.is_some() { "  ├ " } else { "  │ " };
                let max_ms = width.saturating_sub(ms_time.len() + 8);
                let ms_display = if ms.summary.len() > max_ms {
                    format!("{}…", &ms.summary[..max_ms.saturating_sub(1)])
                } else {
                    ms.summary.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Gray)),
                    Span::styled("⚑ ", Style::default().fg(Color::Magenta)),
                    Span::styled(ms_display, Style::default().fg(Color::Gray)),
                    Span::styled(format!("  {}", ms_time), Style::default().fg(Color::DarkGray)),
                ]));
            } else {
                // No milestones — show tool+detail
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
                let max_detail = width.saturating_sub(tool.len() + 6);
                let detail_truncated = if detail.len() > max_detail {
                    format!("{}…", &detail[..max_detail.saturating_sub(1)])
                } else {
                    detail.to_string()
                };
                lines.push(Line::from(vec![
                    Span::styled("  ├ ", Style::default().fg(Color::Gray)),
                    Span::styled(tool.to_string(), Style::default().fg(Color::LightBlue)),
                    Span::styled(" ", Style::default()),
                    Span::styled(detail_truncated, Style::default().fg(Color::Gray)),
                ]));
            }

            // Expanded milestones (all except latest)
            if app.expanded == Some(i) {
                let all_milestones = fetch_milestones(conn, &s.session_id);
                for (mi, ms) in all_milestones.iter().enumerate().skip(1) {
                    let ms_time = relative_time(&ms.created_at);
                    let max_ms = width.saturating_sub(ms_time.len() + 10);
                    let ms_display = if ms.summary.len() > max_ms {
                        format!("{}…", &ms.summary[..max_ms.saturating_sub(1)])
                    } else {
                        ms.summary.clone()
                    };
                    let connector = if mi == all_milestones.len() - 1 {
                        "  └ "
                    } else {
                        "  ├ "
                    };
                    lines.push(Line::from(vec![
                        Span::styled(connector, Style::default().fg(Color::Gray)),
                        Span::styled("⚑ ", Style::default().fg(Color::DarkGray)),
                        Span::styled(ms_display, Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("  {}", ms_time),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }

            if i == app.selected {
                ListItem::new(lines)
                    .style(Style::default().bg(Color::Black).fg(Color::White))
            } else {
                ListItem::new(lines)
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
        Span::raw(" 1-9/C-a..z").bold(),
        Span::raw(" jump  "),
        Span::raw("j/k").bold(),
        Span::raw(" navigate  "),
        Span::raw("o").bold(),
        Span::raw(" expand  "),
        Span::raw("Enter").bold(),
        Span::raw(" switch  "),
        Span::raw("d").bold(),
        Span::raw(" delete  "),
        Span::raw("q").bold(),
        Span::raw(" quit  "),
    ])
    .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(Paragraph::new(footer), chunks[1]);
}

fn run_tui(conn: &Connection, quit_on_select: bool) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.refresh(conn);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui(f, &app, conn))?;

        let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
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
                        KeyCode::Char('j') | KeyCode::Down => {
                            if !ctrl && !app.sessions.is_empty() {
                                app.selected = (app.selected + 1) % app.sessions.len();
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if !ctrl && !app.sessions.is_empty() {
                                app.selected = if app.selected == 0 {
                                    app.sessions.len() - 1
                                } else {
                                    app.selected - 1
                                };
                            }
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
                        KeyCode::Char('d') if !ctrl => {
                            if let Some(session) = app.selected_session() {
                                let id = session.session_id.clone();
                                delete_session(conn, &id);
                                app.refresh(conn);
                            }
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let conn = open_db()?;

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("cj - Claude Jam: monitor and manage Claude Code sessions");
        println!();
        println!("Usage:");
        println!("  cj                   Launch TUI dashboard");
        println!("  cj -q                Launch TUI, quit after selecting a session");
        println!("  cj init <topic>      Pre-register session with topic (before Claude starts)");
        println!("  cj topic <text>      Set topic for current session");
        println!("  cj milestone <text>  Add milestone to current session");
        println!("  cj hook              Process hook event from stdin (used by claude-jam.sh)");
        println!();
        println!("TUI keys:");
        println!("  j/k        Navigate sessions");
        println!("  1-9        Jump to session by number");
        println!("  Ctrl-a..z  Jump to session by letter (after 9)");
        println!("  Enter      Switch to session's tmux session");
        println!("  o          Expand/collapse milestone history");
        println!("  d          Delete session");
        println!("  q          Quit");
        return Ok(());
    }

    match args.get(1).map(|s| s.as_str()) {
        Some("hook") => {
            cmd_hook(&conn);
        }
        Some("init") => {
            let text = args[2..].join(" ");
            if text.is_empty() {
                eprintln!("Usage: cj init <topic>");
                std::process::exit(1);
            }
            cmd_init(&conn, &text);
        }
        Some("topic") => {
            let text = args[2..].join(" ");
            if text.is_empty() {
                eprintln!("Usage: cj topic <description>");
                std::process::exit(1);
            }
            cmd_topic(&conn, &text);
        }
        Some("milestone") => {
            let text = args[2..].join(" ");
            if text.is_empty() {
                eprintln!("Usage: cj milestone <description>");
                std::process::exit(1);
            }
            cmd_milestone(&conn, &text);
        }
        _ => {
            let quit_on_select = args.iter().any(|a| a == "-q" || a == "--quit");
            run_tui(&conn, quit_on_select)?;
        }
    }

    Ok(())
}
