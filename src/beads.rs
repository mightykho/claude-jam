//! Optional Beads (`bd`) integration.
//!
//! Beads is a git-backed issue tracker for AI coding agents
//! (https://github.com/steveyegge/beads). When `bd` is on PATH and the cj
//! session's cwd is inside a `.beads/`-marked project, cj can pull two
//! pieces of context from it:
//!
//! - The current in-progress issue, used as the session topic when the user
//!   hasn't set one manually.
//! - The title of an arbitrary issue id, used to render structured milestones
//!   recorded via `cj milestone --bead <id>`.
//!
//! Every function here is a no-op (returns `None`) when bd isn't installed
//! or the cwd isn't inside a beads project, so cj works identically with or
//! without beads available. No hard dependency.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

/// Maximum time we wait on a `bd` invocation before giving up. Keeps the
/// `cj hook` path from stalling when bd is slow or unresponsive.
const BD_TIMEOUT_MS: u64 = 1500;

/// Subset of the fields `bd show ... --json` emits per issue. Anything not
/// listed here is silently ignored by serde, so bd schema additions don't
/// break us.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BeadsIssue {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: String,
}

impl BeadsIssue {
    /// Render as `"id · title"` — the form cj puts in the topic column.
    pub fn topic_label(&self) -> String {
        if self.title.is_empty() {
            self.id.clone()
        } else {
            format!("{} · {}", self.id, self.title)
        }
    }
}

/// True if `bd` resolves to an executable on PATH.
pub fn is_available() -> bool {
    Command::new("bd")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Walk up from `cwd` looking for a `.beads/` directory. Returns the
/// containing directory (i.e. the beads project root), or `None`.
///
/// bd itself walks up to find `.beads/` when invoked from a subdir, so this
/// function is only used to GATE whether we shell out at all — saves the
/// process spawn when we're definitely not in a beads project.
pub fn find_beads_root(cwd: &Path) -> Option<PathBuf> {
    let mut p: PathBuf = cwd.canonicalize().ok()?;
    loop {
        if p.join(".beads").is_dir() {
            return Some(p);
        }
        if !p.pop() {
            return None;
        }
    }
}

/// `true` when `bd` is on PATH AND `cwd` is inside a beads project — i.e.
/// when the integration is actually applicable.
pub fn active_in(cwd: &Path) -> bool {
    find_beads_root(cwd).is_some() && is_available()
}

/// Return the currently-active beads issue per `bd show --current --json`.
///
/// "Current" per beads docs means "in-progress, hooked, or last touched."
/// Returns `None` when bd isn't installed, the cwd isn't a beads project,
/// the command fails, or beads reports no current issue.
pub fn current_issue(cwd: &Path) -> Option<BeadsIssue> {
    if !active_in(cwd) {
        return None;
    }
    let output = run_bd(cwd, &["show", "--current", "--json"])?;
    parse_issue_response(&output)
}

/// Look up a specific issue by id via `bd show <id> --json`. Same caveats as
/// `current_issue`.
pub fn show_issue(cwd: &Path, id: &str) -> Option<BeadsIssue> {
    if !active_in(cwd) {
        return None;
    }
    let output = run_bd(cwd, &["show", id, "--json"])?;
    parse_issue_response(&output)
}

fn run_bd(cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    // Spawn so we can enforce a timeout. `Command::output()` blocks
    // indefinitely, which is unsafe inside the cj hook path.
    let mut child = Command::new("bd")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + Duration::from_millis(BD_TIMEOUT_MS);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

/// Parse one of two shapes bd's `--json` flag emits:
///   * Array of issue objects: `[{id, title, status, ...}, ...]` — happy path
///   * Error object: `{"error": "...", "schema_version": 1}` — no result
///
/// Always returns the first issue from the array, or `None` for the error
/// shape and any other unexpected JSON.
pub fn parse_issue_response(bytes: &[u8]) -> Option<BeadsIssue> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if v.get("error").is_some() {
        return None;
    }
    let arr = v.as_array()?;
    let first = arr.first()?.clone();
    serde_json::from_value(first).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_returns_first_issue_from_array() {
        let payload = br#"[
            {"id": "bd-test-u81", "title": "wire up auth", "status": "in_progress", "priority": 2}
        ]"#;
        let issue = parse_issue_response(payload).unwrap();
        assert_eq!(issue.id, "bd-test-u81");
        assert_eq!(issue.title, "wire up auth");
        assert_eq!(issue.status, "in_progress");
    }

    #[test]
    fn parse_returns_none_on_error_object() {
        let payload = br#"{"error": "no current issue found", "schema_version": 1}"#;
        assert!(parse_issue_response(payload).is_none());
    }

    #[test]
    fn parse_returns_none_on_empty_array() {
        assert!(parse_issue_response(b"[]").is_none());
    }

    #[test]
    fn parse_returns_none_on_garbage() {
        assert!(parse_issue_response(b"not json").is_none());
        assert!(parse_issue_response(b"").is_none());
    }

    #[test]
    fn parse_tolerates_unknown_fields_and_missing_status() {
        let payload = br#"[
            {"id": "bd-x-001", "title": "x", "some_new_field": 42}
        ]"#;
        let issue = parse_issue_response(payload).unwrap();
        assert_eq!(issue.id, "bd-x-001");
        assert_eq!(issue.status, "");
    }

    #[test]
    fn parse_returns_none_when_id_or_title_missing() {
        // serde rejects the issue object because `id` and `title` are required.
        let payload = br#"[{"status": "open"}]"#;
        assert!(parse_issue_response(payload).is_none());
    }

    #[test]
    fn topic_label_uses_id_only_when_title_is_empty() {
        let issue = BeadsIssue {
            id: "bd-42".into(),
            title: "".into(),
            status: "in_progress".into(),
        };
        assert_eq!(issue.topic_label(), "bd-42");
    }

    #[test]
    fn topic_label_joins_id_and_title_with_separator() {
        let issue = BeadsIssue {
            id: "bd-42".into(),
            title: "wire up auth".into(),
            status: "in_progress".into(),
        };
        assert_eq!(issue.topic_label(), "bd-42 · wire up auth");
    }

    #[test]
    fn find_beads_root_returns_self_when_directly_marked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".beads")).unwrap();
        let found = find_beads_root(dir.path()).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn find_beads_root_walks_up_from_subdirectory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".beads")).unwrap();
        let nested = root.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_beads_root(&nested).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            root.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn find_beads_root_returns_none_when_not_in_a_project() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_beads_root(dir.path()).is_none());
    }
}
