//! Claude Code hook event processing.
//!
//! Every Claude Code lifecycle event (`SessionStart`, `PreToolUse`,
//! `PostToolUse`, `Notification`, `Stop`, …) hits `cj hook` over stdin as a
//! JSON payload. This module turns that payload into database writes:
//! upserting the session row, parsing the transcript for context-window
//! usage, and adopting any pending `tmux:<name>` placeholder.

use rusqlite::Connection;

use crate::db::placeholder::adopt_placeholder;
use crate::models::HookInput;

/// Map a Claude Code event name to the status string we store on the session row.
///
/// `working` is the default for unknown events because new event types
/// generally indicate "Claude is doing something" rather than "Claude is done."
pub fn event_to_status(event: &str) -> &'static str {
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

/// Pick the single most informative field from a hook payload for the dashboard's
/// "detail" column. Truncates to 200 chars so a runaway prompt doesn't blow up
/// the row. Falls back through tool args → prompt → notification message.
pub fn extract_detail(input: &HookInput) -> String {
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

/// Read the most recent assistant turn out of a Claude Code transcript and
/// return `(used_tokens, context_total)`.
///
/// `used` = sum of `input + cache_creation + cache_read + output` from the
/// latest assistant message. `total` defaults to 200K and bumps to 1M once
/// observed usage crosses 200K — the only signal we have, since the transcript
/// doesn't expose the model's actual context window.
///
/// Returns `None` for missing files, empty files, or transcripts with no
/// assistant messages.
pub fn read_context_from_transcript(path: &str) -> Option<(i64, i64)> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut latest: Option<i64> = None;
    for line in content.lines().rev() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let usage = match v.get("message").and_then(|m| m.get("usage")) {
            Some(u) => u,
            None => continue,
        };
        let input = usage
            .get("input_tokens")
            .and_then(|n| n.as_i64())
            .unwrap_or(0);
        let cache_c = usage
            .get("cache_creation_input_tokens")
            .and_then(|n| n.as_i64())
            .unwrap_or(0);
        let cache_r = usage
            .get("cache_read_input_tokens")
            .and_then(|n| n.as_i64())
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(|n| n.as_i64())
            .unwrap_or(0);
        latest = Some(input + cache_c + cache_r + output);
        break;
    }
    let used = latest?;
    let total = if used > 200_000 { 1_000_000 } else { 200_000 };
    Some((used, total))
}

/// End-to-end hook handler: parse the JSON payload, upsert the session row,
/// update context usage from the transcript, and adopt any pending placeholder.
///
/// Silently no-ops on payloads that lack a `session_id` — that's how Claude
/// Code signals events not tied to a specific session (e.g. server-level hooks).
pub fn process_hook_event(conn: &Connection, input_str: &str, tmux_session: &str) {
    let input: HookInput = serde_json::from_str(input_str).unwrap_or_default();

    let session_id = match input.session_id {
        Some(ref id) if !id.is_empty() => id.clone(),
        _ => return,
    };

    let event = input.hook_event_name.as_deref().unwrap_or("");
    let status = event_to_status(event);
    let tool = input.tool_name.as_deref().unwrap_or("");
    let detail = extract_detail(&input);
    let cwd = input.cwd.as_deref().unwrap_or("");

    // Upsert the session row — preserve existing topic.
    conn.execute(
        "INSERT INTO sessions (session_id, status, tool_name, detail, cwd, tmux_session, started_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(session_id) DO UPDATE SET status=excluded.status, tool_name=excluded.tool_name, detail=excluded.detail, cwd=excluded.cwd, tmux_session=excluded.tmux_session, updated_at=CURRENT_TIMESTAMP",
        rusqlite::params![session_id, status, tool, detail, cwd, tmux_session],
    )
    .unwrap();

    if let Some(ref tp) = input.transcript_path {
        if let Some((used, total)) = read_context_from_transcript(tp) {
            let _ = conn.execute(
                "UPDATE sessions SET context_used = ?1, context_total = MAX(COALESCE(context_total, 0), ?2) WHERE session_id = ?3",
                rusqlite::params![used, total, session_id],
            );
        }
    }

    // Adopt any placeholder (`tmux:<name>`) sitting on this tmux session,
    // whether it was created by `cj init` or `cj import`. Runs on every event
    // so an already-running Claude session imported via `cj import` clears its
    // placeholder on the next hook fire — not only on SessionStart.
    adopt_placeholder(conn, &session_id, tmux_session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_transcript(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", content).unwrap();
        f
    }

    #[test]
    fn event_to_status_maps_known_events() {
        assert_eq!(event_to_status("SessionStart"), "idle");
        assert_eq!(event_to_status("UserPromptSubmit"), "working");
        assert_eq!(event_to_status("PreToolUse"), "working");
        assert_eq!(event_to_status("PostToolUse"), "working");
        assert_eq!(event_to_status("Notification"), "waiting");
        assert_eq!(event_to_status("Stop"), "idle");
        assert_eq!(event_to_status("SessionEnd"), "offline");
    }

    #[test]
    fn event_to_status_falls_back_to_working() {
        assert_eq!(event_to_status("UnknownEvent"), "working");
        assert_eq!(event_to_status(""), "working");
    }

    #[test]
    fn extract_detail_prefers_command_then_file_then_pattern() {
        let input = HookInput {
            tool_input: Some(serde_json::json!({"command": "ls -la"})),
            ..Default::default()
        };
        assert_eq!(extract_detail(&input), "ls -la");

        let input = HookInput {
            tool_input: Some(serde_json::json!({"file_path": "/src/foo.rs"})),
            ..Default::default()
        };
        assert_eq!(extract_detail(&input), "/src/foo.rs");

        let input = HookInput {
            tool_input: Some(serde_json::json!({"pattern": "TODO"})),
            ..Default::default()
        };
        assert_eq!(extract_detail(&input), "TODO");
    }

    #[test]
    fn extract_detail_falls_back_to_prompt_then_message() {
        let input = HookInput {
            prompt: Some("explain this".into()),
            ..Default::default()
        };
        assert_eq!(extract_detail(&input), "explain this");

        let input = HookInput {
            message: Some("waiting for input".into()),
            ..Default::default()
        };
        assert_eq!(extract_detail(&input), "waiting for input");
    }

    #[test]
    fn extract_detail_truncates_to_200_chars() {
        let input = HookInput {
            prompt: Some("a".repeat(500)),
            ..Default::default()
        };
        assert_eq!(extract_detail(&input).chars().count(), 200);
    }

    #[test]
    fn read_context_returns_none_for_missing_file() {
        assert!(read_context_from_transcript("/nonexistent/path/xyz").is_none());
    }

    #[test]
    fn read_context_returns_none_for_empty_file() {
        let f = write_transcript("");
        assert!(read_context_from_transcript(f.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn read_context_skips_non_assistant_entries() {
        let content = r#"{"type":"user","message":{"role":"user","content":"hi"}}
{"type":"system","subtype":"start"}"#;
        let f = write_transcript(content);
        assert!(read_context_from_transcript(f.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn read_context_uses_latest_assistant_usage() {
        let content = r#"{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":1000,"output_tokens":5}}}
{"type":"assistant","message":{"usage":{"input_tokens":1,"cache_creation_input_tokens":2,"cache_read_input_tokens":50000,"output_tokens":100}}}"#;
        let f = write_transcript(content);
        let (used, total) = read_context_from_transcript(f.path().to_str().unwrap()).unwrap();
        assert_eq!(used, 50103);
        assert_eq!(total, 200_000);
    }

    #[test]
    fn read_context_bumps_to_1m_when_over_200k() {
        let content = r#"{"type":"assistant","message":{"usage":{"input_tokens":1,"cache_creation_input_tokens":2,"cache_read_input_tokens":300000,"output_tokens":100}}}"#;
        let f = write_transcript(content);
        let (used, total) = read_context_from_transcript(f.path().to_str().unwrap()).unwrap();
        assert!(used > 200_000);
        assert_eq!(total, 1_000_000);
    }

    #[test]
    fn read_context_tolerates_malformed_lines() {
        let content = "not json\n{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":42}}}\nalso not json\n";
        let f = write_transcript(content);
        let (used, _) = read_context_from_transcript(f.path().to_str().unwrap()).unwrap();
        assert_eq!(used, 42);
    }
}
