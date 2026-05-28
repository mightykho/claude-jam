//! tmux shell-out helpers — everything cj needs to interact with the running tmux server.
//!
//! All functions are best-effort: if tmux isn't running, the binaries aren't on PATH,
//! or commands fail, we return `None`/no-op rather than panicking, so cj can still run
//! in non-tmux contexts (CI, container builds, etc.) and degrade gracefully.

use std::process::Command;

/// Returns the current tmux session name (`#S`), or `None` if not in a tmux session.
pub fn current_tmux_session() -> Option<String> {
    Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
}

/// Returns every tmux session name visible to the running server, or `None` if
/// tmux isn't available. Used by `cj import`.
pub fn list_tmux_sessions() -> Option<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// Switch the active tmux client to `session_name`. No-op outside tmux.
pub fn switch_tmux_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["switch-client", "-t", session_name])
        .status();
}
