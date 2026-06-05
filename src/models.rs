//! Shared data types used across the lib + bin boundary.

use serde::Deserialize;

/// A row in the `sessions` table, as returned by [`crate::db::fetch_sessions`].
pub struct Session {
    pub session_id: String,
    pub status: String,
    pub tool_name: Option<String>,
    pub detail: Option<String>,
    #[allow(dead_code)]
    pub cwd: Option<String>,
    pub tmux_session: Option<String>,
    pub topic: Option<String>,
    pub updated_at: String,
    pub context_used: Option<i64>,
    pub context_total: Option<i64>,
}

/// A row in the `milestones` table.
pub struct Milestone {
    pub summary: String,
    pub created_at: String,
    /// Optional beads issue id this milestone was recorded against (e.g.
    /// `bd-42`), via `cj milestone --bead <id>`. `None` for free-form
    /// milestones.
    pub bead_ref: Option<String>,
}

/// JSON payload Claude Code passes to every lifecycle hook over stdin.
///
/// Only the fields cj actually reads are deserialized; everything else is
/// silently ignored by serde.
#[derive(Deserialize, Default, Debug, Clone)]
pub struct HookInput {
    pub session_id: Option<String>,
    pub hook_event_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub cwd: Option<String>,
    pub prompt: Option<String>,
    pub message: Option<String>,
    pub transcript_path: Option<String>,
}
