//! SQLite storage layer.
//!
//! Two tables (`sessions`, `milestones`) live in `~/.claude/claude-jam.db`,
//! shared by every cj invocation — the TUI, the hook, and the CLI commands
//! all open the same file. WAL mode keeps concurrent reads + writes safe.

use std::path::PathBuf;

use rusqlite::Connection;

use crate::models::{Milestone, Session};

pub mod placeholder;

use placeholder::cleanup_orphan_placeholders;

/// Resolve the path to the shared database. Honours `$HOME`; falls back to
/// `./.claude/claude-jam.db` if `$HOME` is missing.
pub fn db_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".claude")
        .join("claude-jam.db")
}

/// Apply the schema (idempotently) and any pending column migrations.
///
/// Migrations use `ALTER TABLE ... ADD COLUMN` wrapped in `let _ =` so re-running
/// against an already-migrated database silently no-ops on the duplicate-column
/// error instead of panicking.
pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
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
    let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN topic TEXT;");
    let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN context_used INTEGER;");
    let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN context_total INTEGER;");
    Ok(())
}

/// Open the shared database, apply schema migrations, and clear any orphaned
/// `tmux:<name>` placeholders left over from prior cj invocations.
///
/// Creates the parent directory (typically `~/.claude/`) if it doesn't exist
/// yet, so a fresh system invocation succeeds without requiring the user to
/// have run `cj setup` first.
pub fn open_db() -> rusqlite::Result<Connection> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(&path)?;
    init_schema(&conn)?;
    cleanup_orphan_placeholders(&conn);
    Ok(conn)
}

/// All active (`status != 'offline'`) sessions, newest first.
pub fn fetch_sessions(conn: &Connection) -> Vec<Session> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, status, tool_name, detail, cwd, tmux_session, topic, updated_at, context_used, context_total
             FROM sessions
             WHERE status != 'offline'
             ORDER BY started_at DESC",
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
            context_used: row.get(8)?,
            context_total: row.get(9)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Most recent milestone for a given session, or `None`.
pub fn fetch_latest_milestone(conn: &Connection, session_id: &str) -> Option<Milestone> {
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

/// Every milestone for a session, newest first.
pub fn fetch_milestones(conn: &Connection, session_id: &str) -> Vec<Milestone> {
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

/// Delete a session and all its milestones. Idempotent.
pub fn delete_session(conn: &Connection, session_id: &str) {
    let _ = conn.execute("DELETE FROM milestones WHERE session_id = ?1", [session_id]);
    let _ = conn.execute("DELETE FROM sessions WHERE session_id = ?1", [session_id]);
}

/// Most recent non-offline session for a tmux name, if any. Used to map a
/// `cj topic`/`cj milestone` call in a tmux pane back to its session row.
pub fn find_session_by_tmux(conn: &Connection, tmux_name: &str) -> Option<String> {
    conn.query_row(
        "SELECT session_id FROM sessions WHERE tmux_session = ?1 AND status != 'offline' ORDER BY updated_at DESC LIMIT 1",
        [tmux_name],
        |row| row.get(0),
    )
    .ok()
}

/// Read the context-window usage stored for a session. Returns `None` when
/// either the row is missing or the columns haven't been populated yet.
pub fn db_get_context(conn: &Connection, session_id: &str) -> Option<(i64, i64)> {
    conn.query_row(
        "SELECT context_used, context_total FROM sessions WHERE session_id = ?1",
        [session_id],
        |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
    )
    .ok()
    .and_then(|(u, t)| Some((u?, t?)))
}

/// Insert a `tmux:<name>` placeholder row for any tmux session not already
/// tracked by cj. Returns `(imported, skipped)` so callers can report progress.
pub fn db_import_tmux_sessions(
    conn: &Connection,
    tmux_names: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut imported: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for tmux in tmux_names {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE tmux_session = ?1 AND status != 'offline'",
                [tmux],
                |r| r.get(0),
            )
            .unwrap_or(0);

        if count > 0 {
            skipped.push(tmux.clone());
            continue;
        }

        let placeholder_id = format!("tmux:{}", tmux);
        let affected = conn
            .execute(
                "INSERT INTO sessions (session_id, status, tmux_session, started_at, updated_at)
                 VALUES (?1, 'pending', ?2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(session_id) DO NOTHING",
                rusqlite::params![placeholder_id, tmux],
            )
            .unwrap_or(0);
        if affected > 0 {
            imported.push(tmux.clone());
        } else {
            skipped.push(tmux.clone());
        }
    }

    (imported, skipped)
}
