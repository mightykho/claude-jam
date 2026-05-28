//! `tmux:<name>` placeholder rows and their cleanup.
//!
//! `cj init` and `cj import` create placeholders so a tmux session shows up in
//! the dashboard before Claude actually starts in it. As soon as the real
//! Claude Code session fires its first hook event, the placeholder needs to be
//! folded into the real row (topic + milestones) and then deleted.

use rusqlite::Connection;

/// Migrate a `tmux:<name>` placeholder row onto a real session row.
///
/// Moves milestones from placeholder to real, copies the placeholder's topic
/// only if the real row has none (preserving an existing user-set topic), and
/// deletes the placeholder. No-op when:
///
/// - `tmux_session` is empty (no placeholder id to construct)
/// - `real_session_id` is itself the placeholder id (don't self-delete)
/// - The placeholder doesn't exist
pub fn adopt_placeholder(conn: &Connection, real_session_id: &str, tmux_session: &str) {
    if tmux_session.is_empty() {
        return;
    }
    let placeholder_id = format!("tmux:{}", tmux_session);
    if placeholder_id == real_session_id {
        return;
    }
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sessions WHERE session_id = ?1",
            [&placeholder_id],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return;
    }
    let _ = conn.execute(
        "UPDATE milestones SET session_id = ?1 WHERE session_id = ?2",
        rusqlite::params![real_session_id, placeholder_id],
    );
    let _ = conn.execute(
        "UPDATE sessions
         SET topic = COALESCE(topic, (SELECT topic FROM sessions WHERE session_id = ?1))
         WHERE session_id = ?2",
        rusqlite::params![placeholder_id, real_session_id],
    );
    let _ = conn.execute(
        "DELETE FROM sessions WHERE session_id = ?1",
        [&placeholder_id],
    );
}

/// Drop any `tmux:<name>` placeholder that has been superseded by a real
/// session row sharing the same `tmux_session`. Runs on every `open_db` so
/// stale placeholders from earlier sessions self-heal without manual cleanup.
/// Idempotent — does nothing once the database is clean.
pub fn cleanup_orphan_placeholders(conn: &Connection) {
    let mut stmt = match conn.prepare(
        "SELECT session_id, tmux_session FROM sessions
         WHERE session_id LIKE 'tmux:%' AND tmux_session IS NOT NULL AND tmux_session != ''",
    ) {
        Ok(s) => s,
        Err(_) => return,
    };
    let placeholders: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    for (placeholder_id, tmux_name) in placeholders {
        let real_id: Option<String> = conn
            .query_row(
                "SELECT session_id FROM sessions
                 WHERE session_id != ?1 AND tmux_session = ?2
                 ORDER BY updated_at DESC LIMIT 1",
                rusqlite::params![placeholder_id, tmux_name],
                |r| r.get(0),
            )
            .ok();
        if let Some(real_id) = real_id {
            adopt_placeholder(conn, &real_id, &tmux_name);
        }
    }
}
