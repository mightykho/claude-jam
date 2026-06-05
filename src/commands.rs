//! CLI subcommand handlers.
//!
//! Each `cmd_*` is the binary-side wrapper around a `claude_jam::*` helper:
//! it handles tmux/argument resolution, prints user-facing output, and
//! exits with a non-zero status on user errors. Library code stays exit-free
//! and non-printing so it can be tested cleanly.

use std::io::Read as _;

use rusqlite::Connection;

use claude_jam::beads;
use claude_jam::db::{
    add_milestone, db_get_context, db_import_tmux_sessions, delete_session, find_session_by_tmux,
    get_session_cwd,
};
use claude_jam::hook::process_hook_event;
use claude_jam::setup::{self, Action, Report};
use claude_jam::tmux::{current_tmux_session, list_tmux_sessions};

/// Translate a CLI invocation into the session_id it should target.
///
/// `--session-id` short-circuits everything. Otherwise we look up the current
/// tmux session and find the most recent real session row for it. Exits with
/// status 1 (and a stderr message) if neither path produces a session id.
pub fn resolve_session_id(conn: &Connection, session_id_override: Option<&str>) -> String {
    if let Some(id) = session_id_override {
        return id.to_string();
    }
    let tmux = match current_tmux_session() {
        Some(t) => t,
        None => {
            eprintln!("Error: not in a tmux session and no --session-id provided");
            std::process::exit(1);
        }
    };
    match find_session_by_tmux(conn, &tmux) {
        Some(id) => id,
        None => {
            eprintln!("Error: no active session found for tmux session '{}'", tmux);
            std::process::exit(1);
        }
    }
}

pub fn cmd_topic(conn: &Connection, session_id_override: Option<&str>, text: &str) {
    let session_id = resolve_session_id(conn, session_id_override);
    // Mark topic_source='manual' so the beads-driven topic refresh in the
    // hook path doesn't clobber a topic the user / agent set explicitly.
    conn.execute(
        "UPDATE sessions SET topic = ?1, topic_source = 'manual' WHERE session_id = ?2",
        rusqlite::params![text, session_id],
    )
    .unwrap();
    println!("Topic set for session {}", session_id);
}

pub fn cmd_milestone(
    conn: &Connection,
    session_id_override: Option<&str>,
    bead_id: Option<&str>,
    text: &str,
) {
    let session_id = resolve_session_id(conn, session_id_override);

    let (summary, bead_ref) = if let Some(id) = bead_id {
        // Resolve cwd: prefer the session's stored cwd (where the agent is
        // actually working), fall back to the caller's cwd. Either should
        // be inside the same beads project in practice.
        let cwd = get_session_cwd(conn, &session_id)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let issue = beads::show_issue(&cwd, id);
        let summary = match (issue, text.is_empty()) {
            (Some(i), true) => i.title,
            (Some(i), false) => format!("{}: {}", i.title, text),
            (None, true) => {
                // bd unavailable or issue missing — record bare id so the
                // milestone still lands with a useful reference.
                id.to_string()
            }
            (None, false) => text.to_string(),
        };
        (summary, Some(id))
    } else {
        if text.is_empty() {
            eprintln!("Usage: cj milestone [--session-id <id>] [--bead <bead-id>] <text>");
            std::process::exit(1);
        }
        (text.to_string(), None)
    };

    add_milestone(conn, &session_id, &summary, bead_ref).unwrap();
    if let Some(id) = bead_ref {
        println!("Milestone added for session {} (bd ref {})", session_id, id);
    } else {
        println!("Milestone added for session {}", session_id);
    }
}

pub fn cmd_context(conn: &Connection, session_id_override: Option<&str>) {
    let session_id = resolve_session_id(conn, session_id_override);
    match db_get_context(conn, &session_id) {
        Some((used, total)) => println!("{}/{}", used, total),
        None => {
            eprintln!("No context info available for session {}", session_id);
            std::process::exit(1);
        }
    }
}

pub fn cmd_init(conn: &Connection, tmux_override: Option<&str>, topic: &str) {
    let tmux = match tmux_override {
        Some(t) => t.to_string(),
        None => match current_tmux_session() {
            Some(t) => t,
            None => {
                eprintln!("Error: not in a tmux session and no -s flag provided");
                std::process::exit(1);
            }
        },
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

pub fn cmd_import(conn: &Connection) {
    let tmux_sessions = match list_tmux_sessions() {
        Some(s) => s,
        None => {
            eprintln!("Error: failed to list tmux sessions (is tmux running?)");
            std::process::exit(1);
        }
    };

    if tmux_sessions.is_empty() {
        println!("No tmux sessions found");
        return;
    }

    let (imported, skipped) = db_import_tmux_sessions(conn, &tmux_sessions);

    if !imported.is_empty() {
        println!("Imported {} session(s):", imported.len());
        for t in &imported {
            println!("  + {}", t);
        }
    }
    if !skipped.is_empty() {
        println!("Skipped {} session(s) already tracked", skipped.len());
    }
}

pub fn cmd_remove(conn: &Connection, tmux_name: &str) {
    let mut stmt = conn
        .prepare("SELECT session_id FROM sessions WHERE tmux_session = ?1")
        .unwrap();
    let ids: Vec<String> = stmt
        .query_map([tmux_name], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    if ids.is_empty() {
        println!("No sessions found for tmux session '{}'", tmux_name);
        return;
    }
    for id in &ids {
        delete_session(conn, id);
    }
    // Also remove any placeholder
    let placeholder_id = format!("tmux:{}", tmux_name);
    delete_session(conn, &placeholder_id);
    println!("Removed {} session(s) for '{}'", ids.len(), tmux_name);
}

fn print_report(report: &Report) {
    for action in &report.actions {
        println!("  [{}] {}", action.glyph(), action.label());
    }
    if !report.changed() {
        println!("\nNo changes needed.");
    }
}

pub fn cmd_setup(check_only: bool) {
    let dir = setup::default_claude_dir();
    let result = if check_only {
        setup::check(&dir)
    } else {
        setup::install(&dir)
    };
    match result {
        Ok(report) => {
            let header = if check_only {
                format!("Claude Code setup status under {}:", dir.display())
            } else {
                format!("Wiring Claude Jam into {}:", dir.display())
            };
            println!("{header}");
            print_report(&report);
            if !check_only && report.changed() {
                println!(
                    "\nDone. Start a fresh Claude Code session and cj will begin tracking it."
                );
            }
            if check_only
                && report
                    .actions
                    .iter()
                    .any(|a| matches!(a, Action::NotPresent(_)))
            {
                println!("\nRun `cj setup` to apply the missing items.");
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_teardown() {
    let dir = setup::default_claude_dir();
    match setup::uninstall(&dir) {
        Ok(report) => {
            println!("Removing Claude Jam wiring from {}:", dir.display());
            print_report(&report);
            if report.changed() {
                println!(
                    "\nDone. The SQLite database at {}/claude-jam.db is preserved \u{2014} \
                     delete it manually if you want a clean slate.",
                    dir.display()
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_hook(conn: &Connection) {
    let mut input_str = String::new();
    std::io::stdin().read_to_string(&mut input_str).unwrap_or(0);
    let tmux_session = current_tmux_session().unwrap_or_default();
    process_hook_event(conn, &input_str, &tmux_session);
}
