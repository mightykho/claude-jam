//! End-to-end tests that exercise the DB + hook layer together against an
//! in-memory SQLite. Pure-function tests live alongside their modules in the
//! lib; this file owns the cross-module flows.

use std::io::Write;

use claude_jam::db::placeholder::cleanup_orphan_placeholders;
use claude_jam::db::{
    add_milestone, db_get_context, db_import_tmux_sessions, fetch_latest_milestone,
    fetch_milestones, init_schema,
};
use claude_jam::hook::process_hook_event;
use rusqlite::Connection;

fn fresh_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    conn
}

fn insert_session(conn: &Connection, session_id: &str, tmux: &str, status: &str) {
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, started_at, updated_at)
         VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        rusqlite::params![session_id, status, tmux],
    )
    .unwrap();
}

fn write_transcript(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    write!(f, "{}", content).unwrap();
    f
}

// ----- schema -----

#[test]
fn init_schema_creates_required_tables() {
    let conn = fresh_db();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert!(tables.contains(&"sessions".to_string()));
    assert!(tables.contains(&"milestones".to_string()));
}

#[test]
fn init_schema_creates_context_columns() {
    let conn = fresh_db();
    conn.execute(
        "INSERT INTO sessions (session_id, status, context_used, context_total) VALUES ('s1', 'idle', 100, 200)",
        [],
    )
    .unwrap();
    let (u, t): (i64, i64) = conn
        .query_row(
            "SELECT context_used, context_total FROM sessions WHERE session_id='s1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((u, t), (100, 200));
}

// ----- db_get_context -----

#[test]
fn db_get_context_returns_none_when_no_row() {
    let conn = fresh_db();
    assert_eq!(db_get_context(&conn, "missing"), None);
}

#[test]
fn db_get_context_returns_none_when_columns_null() {
    let conn = fresh_db();
    insert_session(&conn, "s1", "tmux1", "working");
    assert_eq!(db_get_context(&conn, "s1"), None);
}

#[test]
fn db_get_context_returns_value_when_populated() {
    let conn = fresh_db();
    conn.execute(
        "INSERT INTO sessions (session_id, status, context_used, context_total) VALUES ('s1', 'idle', 150, 200)",
        [],
    )
    .unwrap();
    assert_eq!(db_get_context(&conn, "s1"), Some((150, 200)));
}

// ----- db_import_tmux_sessions -----

#[test]
fn import_adds_untracked_sessions_as_pending() {
    let conn = fresh_db();
    let names = vec!["alpha".to_string(), "beta".to_string()];
    let (imported, skipped) = db_import_tmux_sessions(&conn, &names);
    assert_eq!(imported.len(), 2);
    assert_eq!(skipped.len(), 0);

    let status: String = conn
        .query_row(
            "SELECT status FROM sessions WHERE session_id='tmux:alpha'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
}

#[test]
fn import_skips_sessions_with_active_tmux_match() {
    let conn = fresh_db();
    insert_session(&conn, "real-uuid-1", "alpha", "working");
    let (imported, skipped) =
        db_import_tmux_sessions(&conn, &["alpha".to_string(), "beta".to_string()]);
    assert_eq!(imported, vec!["beta".to_string()]);
    assert_eq!(skipped, vec!["alpha".to_string()]);
}

#[test]
fn import_is_idempotent() {
    let conn = fresh_db();
    let names = vec!["alpha".to_string()];
    db_import_tmux_sessions(&conn, &names);
    let (imported, skipped) = db_import_tmux_sessions(&conn, &names);
    assert_eq!(imported.len(), 0);
    assert_eq!(skipped.len(), 1);
}

// ----- process_hook_event -----

#[test]
fn hook_ignores_input_without_session_id() {
    let conn = fresh_db();
    process_hook_event(&conn, r#"{"hook_event_name":"PostToolUse"}"#, "tmux1");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn hook_upserts_session_with_tool_and_status() {
    let conn = fresh_db();
    let payload = r#"{
        "session_id": "abc",
        "hook_event_name": "PreToolUse",
        "tool_name": "Read",
        "tool_input": {"file_path": "src/main.rs"},
        "cwd": "/tmp"
    }"#;
    process_hook_event(&conn, payload, "my-tmux");

    let (status, tool, detail, tmux): (String, String, String, String) = conn
        .query_row(
            "SELECT status, tool_name, detail, tmux_session FROM sessions WHERE session_id='abc'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(status, "working");
    assert_eq!(tool, "Read");
    assert_eq!(detail, "src/main.rs");
    assert_eq!(tmux, "my-tmux");
}

#[test]
fn hook_sets_waiting_status_on_notification() {
    let conn = fresh_db();
    let payload = r#"{"session_id":"abc","hook_event_name":"Notification","message":"waiting"}"#;
    process_hook_event(&conn, payload, "my-tmux");
    let status: String = conn
        .query_row(
            "SELECT status FROM sessions WHERE session_id='abc'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "waiting");
}

#[test]
fn hook_adopts_placeholder_topic_on_session_start() {
    let conn = fresh_db();
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, topic, started_at, updated_at)
         VALUES ('tmux:my-tmux', 'pending', 'my-tmux', 'my goal', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();

    let payload = r#"{"session_id":"real-uuid","hook_event_name":"SessionStart"}"#;
    process_hook_event(&conn, payload, "my-tmux");

    let placeholder_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE session_id='tmux:my-tmux'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(placeholder_count, 0);

    let topic: String = conn
        .query_row(
            "SELECT topic FROM sessions WHERE session_id='real-uuid'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(topic, "my goal");
}

#[test]
fn hook_cleans_up_topicless_import_placeholder_on_any_event() {
    let conn = fresh_db();
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, started_at, updated_at)
         VALUES ('tmux:my-tmux', 'pending', 'my-tmux', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();

    let payload = r#"{"session_id":"real-uuid","hook_event_name":"PreToolUse","tool_name":"Read"}"#;
    process_hook_event(&conn, payload, "my-tmux");

    let placeholder_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE session_id='tmux:my-tmux'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(placeholder_count, 0);
}

#[test]
fn hook_migrates_placeholder_milestones_to_real_session() {
    let conn = fresh_db();
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, started_at, updated_at)
         VALUES ('tmux:my-tmux', 'pending', 'my-tmux', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO milestones (session_id, summary) VALUES ('tmux:my-tmux', 'wrote tests')",
        [],
    )
    .unwrap();

    let payload = r#"{"session_id":"real-uuid","hook_event_name":"PostToolUse"}"#;
    process_hook_event(&conn, payload, "my-tmux");

    let owner: String = conn
        .query_row(
            "SELECT session_id FROM milestones WHERE summary='wrote tests'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner, "real-uuid");
}

#[test]
fn hook_does_not_overwrite_existing_real_topic_with_placeholder_topic() {
    let conn = fresh_db();
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, topic, started_at, updated_at)
         VALUES ('real-uuid', 'working', 'my-tmux', 'real topic', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, topic, started_at, updated_at)
         VALUES ('tmux:my-tmux', 'pending', 'my-tmux', 'stale placeholder topic', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();

    let payload = r#"{"session_id":"real-uuid","hook_event_name":"PreToolUse"}"#;
    process_hook_event(&conn, payload, "my-tmux");

    let topic: String = conn
        .query_row(
            "SELECT topic FROM sessions WHERE session_id='real-uuid'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(topic, "real topic");
}

#[test]
fn hook_updates_context_from_transcript() {
    let conn = fresh_db();
    let transcript = write_transcript(
        r#"{"type":"assistant","message":{"usage":{"input_tokens":1,"cache_read_input_tokens":50000,"output_tokens":10}}}"#,
    );
    let payload = format!(
        r#"{{"session_id":"abc","hook_event_name":"PostToolUse","transcript_path":"{}"}}"#,
        transcript.path().to_str().unwrap()
    );
    process_hook_event(&conn, &payload, "my-tmux");
    let (used, total) = db_get_context(&conn, "abc").unwrap();
    assert_eq!(used, 50011);
    assert_eq!(total, 200_000);
}

#[test]
fn hook_context_total_never_decreases() {
    let conn = fresh_db();
    let t1 = write_transcript(
        r#"{"type":"assistant","message":{"usage":{"cache_read_input_tokens":300000}}}"#,
    );
    let payload1 = format!(
        r#"{{"session_id":"abc","hook_event_name":"PostToolUse","transcript_path":"{}"}}"#,
        t1.path().to_str().unwrap()
    );
    process_hook_event(&conn, &payload1, "my-tmux");

    let t2 = write_transcript(
        r#"{"type":"assistant","message":{"usage":{"cache_read_input_tokens":100000}}}"#,
    );
    let payload2 = format!(
        r#"{{"session_id":"abc","hook_event_name":"PostToolUse","transcript_path":"{}"}}"#,
        t2.path().to_str().unwrap()
    );
    process_hook_event(&conn, &payload2, "my-tmux");

    let (_, total) = db_get_context(&conn, "abc").unwrap();
    assert_eq!(total, 1_000_000);
}

// ----- cleanup_orphan_placeholders -----

#[test]
fn cleanup_drops_placeholder_when_real_session_exists() {
    let conn = fresh_db();
    insert_session(&conn, "real-uuid", "my-tmux", "waiting");
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, started_at, updated_at)
         VALUES ('tmux:my-tmux', 'pending', 'my-tmux', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();

    cleanup_orphan_placeholders(&conn);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE tmux_session='my-tmux'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn cleanup_keeps_placeholder_with_no_matching_real_session() {
    let conn = fresh_db();
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, topic, started_at, updated_at)
         VALUES ('tmux:future-tmux', 'pending', 'future-tmux', 'planned work', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();

    cleanup_orphan_placeholders(&conn);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE session_id='tmux:future-tmux'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

// ----- beads integration: milestones with bead_ref -----

#[test]
fn add_milestone_persists_bead_ref_when_supplied() {
    let conn = fresh_db();
    insert_session(&conn, "s1", "tmux1", "working");

    add_milestone(&conn, "s1", "wire up auth", Some("bd-test-u81")).unwrap();
    add_milestone(&conn, "s1", "free-form note", None).unwrap();

    let all = fetch_milestones(&conn, "s1");
    assert_eq!(all.len(), 2);
    // Newest first — the free-form note was inserted second.
    assert_eq!(all[0].summary, "free-form note");
    assert_eq!(all[0].bead_ref, None);
    assert_eq!(all[1].summary, "wire up auth");
    assert_eq!(all[1].bead_ref.as_deref(), Some("bd-test-u81"));
}

#[test]
fn fetch_latest_milestone_includes_bead_ref() {
    let conn = fresh_db();
    insert_session(&conn, "s1", "tmux1", "working");
    add_milestone(&conn, "s1", "closed an issue", Some("bd-42")).unwrap();

    let latest = fetch_latest_milestone(&conn, "s1").unwrap();
    assert_eq!(latest.summary, "closed an issue");
    assert_eq!(latest.bead_ref.as_deref(), Some("bd-42"));
}

#[test]
fn legacy_milestones_without_bead_ref_round_trip_as_none() {
    let conn = fresh_db();
    insert_session(&conn, "s1", "tmux1", "working");
    // Insert via the pre-bead_ref code path (no column listed).
    conn.execute(
        "INSERT INTO milestones (session_id, summary) VALUES ('s1', 'legacy entry')",
        [],
    )
    .unwrap();

    let latest = fetch_latest_milestone(&conn, "s1").unwrap();
    assert_eq!(latest.summary, "legacy entry");
    assert_eq!(latest.bead_ref, None);
}

// ----- beads integration: topic_source semantics -----

#[test]
fn hook_does_not_overwrite_manual_topic_outside_beads_project() {
    let conn = fresh_db();
    // Real session with a manually-set topic.
    conn.execute(
        "INSERT INTO sessions (session_id, status, tmux_session, topic, topic_source, started_at, updated_at)
         VALUES ('s1', 'working', 'my-tmux', 'manual topic', 'manual', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();

    // UserPromptSubmit hook in a non-beads cwd should NOT touch the topic
    // (beads short-circuits on `active_in(cwd)` returning false).
    let payload = r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","cwd":"/tmp"}"#;
    process_hook_event(&conn, payload, "my-tmux");

    let (topic, source): (String, String) = conn
        .query_row(
            "SELECT topic, topic_source FROM sessions WHERE session_id='s1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(topic, "manual topic");
    assert_eq!(source, "manual");
}
