//! Idempotent wiring of cj into Claude Code's user-level config.
//!
//! This module owns everything `install.sh` used to do in shell + Python:
//! creating the hook wrapper, registering hooks for every lifecycle event,
//! granting `Bash(cj:*)` permission, and adding the CLAUDE.md instruction
//! about `cj topic` / `cj milestone`.
//!
//! The logic lives here (rather than in `install.sh`) so it works through
//! every install channel — `brew install`, `cargo install`, prebuilt
//! tarballs, manual clones — not just the git-clone path. Pure JSON
//! manipulation is exposed for unit testing; the orchestration functions
//! (`install` / `uninstall` / `check`) read and write the real files under
//! `claude_dir`.
//!
//! Re-running is safe: every step checks current state and reports
//! `[=] already present` rather than appending duplicate entries.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Path to the hook wrapper script that Claude Code invokes (relative to
/// `claude_dir`). Kept stable across versions so old/new install paths
/// produce compatible state.
pub const HOOK_SCRIPT_RELATIVE: &str = "hooks/claude-jam.sh";

/// Contents of the hook wrapper script. Resolves `cj` via PATH so it works
/// whether cj is installed at `~/bin/cj`, `/opt/homebrew/bin/cj`, or anywhere
/// else.
pub const HOOK_SCRIPT_CONTENT: &str = "#!/bin/bash\nexec cj hook\n";

/// The `command` field that Claude Code stores in `settings.json` for each
/// registered hook event. Uses the literal `~` (Claude Code expands it) so
/// the rendered settings file matches what `install.sh` historically wrote.
pub const HOOK_COMMAND: &str = "~/.claude/hooks/claude-jam.sh";

/// Permission cj asks Claude Code to grant so the agent can run any `cj`
/// subcommand without prompting on each call.
pub const CJ_PERMISSION: &str = "Bash(cj:*)";

/// Every Claude Code lifecycle event cj registers for. cj hook reads the
/// event type from stdin and dispatches per-event.
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// One-line instruction appended to `CLAUDE.md` so the agent learns to call
/// `cj topic` / `cj milestone`. Kept identical to what `install.sh` writes so
/// teardown can match-and-remove it cleanly.
pub const CLAUDE_MD_INSTRUCTION: &str =
    "- Claude Jam (`cj`) tracks session context. When you establish the main goal of a session (after understanding the task, reading a ticket, etc.), run: `cj topic \"concise description of the goal\"`. When you complete a significant step or milestone, run: `cj milestone \"what was accomplished\"`. Keep descriptions short and informative.";

/// Substring used to detect the instruction line above when re-installing or
/// tearing down. The full string changes occasionally; the substring shouldn't.
pub const CLAUDE_MD_MARKER: &str = "Claude Jam (`cj`) tracks session context";

/// Default install root. Honours `CJ_CLAUDE_DIR` for testing, then `$HOME/.claude`.
pub fn default_claude_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CJ_CLAUDE_DIR") {
        return PathBuf::from(p);
    }
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".claude")
}

/// A single change `install` / `uninstall` / `check` performed (or would have).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Added(String),
    AlreadyPresent(String),
    Removed(String),
    NotPresent(String),
}

impl Action {
    /// Single-character prefix for terse output: `+`, `=`, `-`, `·`.
    pub fn glyph(&self) -> char {
        match self {
            Action::Added(_) => '+',
            Action::AlreadyPresent(_) => '=',
            Action::Removed(_) => '-',
            Action::NotPresent(_) => '·',
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Action::Added(s)
            | Action::AlreadyPresent(s)
            | Action::Removed(s)
            | Action::NotPresent(s) => s,
        }
    }

    pub fn is_change(&self) -> bool {
        matches!(self, Action::Added(_) | Action::Removed(_))
    }
}

/// Outcome of a setup / teardown / check run.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub actions: Vec<Action>,
}

impl Report {
    pub fn push(&mut self, a: Action) {
        self.actions.push(a);
    }
    pub fn changed(&self) -> bool {
        self.actions.iter().any(Action::is_change)
    }
}

/// Errors from filesystem / JSON work.
#[derive(Debug)]
pub enum SetupError {
    Io(io::Error),
    JsonParse(serde_json::Error),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Io(e) => write!(f, "I/O error: {e}"),
            SetupError::JsonParse(e) => write!(f, "settings.json is invalid: {e}"),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<io::Error> for SetupError {
    fn from(e: io::Error) -> Self {
        SetupError::Io(e)
    }
}
impl From<serde_json::Error> for SetupError {
    fn from(e: serde_json::Error) -> Self {
        SetupError::JsonParse(e)
    }
}

// =====================================================================
// Pure JSON helpers — operate on a `serde_json::Value` representing
// settings.json. No I/O, easy to unit-test exhaustively.
// =====================================================================

/// Ensure `Bash(cj:*)` is in `permissions.allow`. Returns `true` if added.
pub fn add_permission(settings: &mut Value, perm: &str) -> bool {
    let perms = settings
        .as_object_mut()
        .expect("settings root must be an object")
        .entry("permissions")
        .or_insert_with(|| Value::Object(Default::default()));
    let allow = perms
        .as_object_mut()
        .expect("permissions must be an object")
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = allow
        .as_array_mut()
        .expect("permissions.allow must be array");
    if arr.iter().any(|v| v.as_str() == Some(perm)) {
        false
    } else {
        arr.push(Value::String(perm.to_string()));
        true
    }
}

/// Remove `Bash(cj:*)` from `permissions.allow`. Returns `true` if removed.
pub fn remove_permission(settings: &mut Value, perm: &str) -> bool {
    let Some(arr) = settings
        .pointer_mut("/permissions/allow")
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    let before = arr.len();
    arr.retain(|v| v.as_str() != Some(perm));
    before != arr.len()
}

/// Add the hook command to every event in `events`. Returns the list of
/// events that were modified (events where the hook wasn't already
/// registered).
///
/// Schema preserved:
/// ```json
/// {"hooks": {"PreToolUse": [{"matcher": "", "hooks": [{"type":"command","command":"..."}]}]}}
/// ```
pub fn add_hooks(settings: &mut Value, hook_command: &str, events: &[&str]) -> Vec<String> {
    let mut added = Vec::new();
    let hooks = settings
        .as_object_mut()
        .expect("settings root must be an object")
        .entry("hooks")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .expect("hooks must be an object");

    for event in events {
        let groups = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("hooks event entry must be an array");

        // Already present anywhere in any matcher group?
        let already = groups.iter().any(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .map(|hs| {
                    hs.iter()
                        .any(|h| h.get("command").and_then(Value::as_str) == Some(hook_command))
                })
                .unwrap_or(false)
        });
        if already {
            continue;
        }

        // Find (or create) the catch-all matcher group (matcher == "").
        let catchall_idx = groups.iter().position(|g| {
            g.get("matcher")
                .and_then(Value::as_str)
                .map(str::is_empty)
                .unwrap_or(false)
        });
        let idx = match catchall_idx {
            Some(i) => i,
            None => {
                groups.push(serde_json::json!({ "matcher": "", "hooks": [] }));
                groups.len() - 1
            }
        };
        let target = groups[idx]
            .get_mut("hooks")
            .and_then(Value::as_array_mut)
            .expect("matcher group must have hooks array");
        target.push(serde_json::json!({ "type": "command", "command": hook_command }));
        added.push(event.to_string());
    }
    added
}

/// Remove every entry matching `hook_command` from every event. Also prunes
/// matcher groups that become empty as a result and event keys that lose all
/// their groups, so a fresh teardown leaves no cj-shaped detritus behind.
/// Returns the list of events that lost an entry.
pub fn remove_hooks(settings: &mut Value, hook_command: &str, events: &[&str]) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(hooks) = settings
        .pointer_mut("/hooks")
        .and_then(Value::as_object_mut)
    else {
        return removed;
    };
    let mut empty_events: Vec<String> = Vec::new();
    for event in events {
        let Some(groups) = hooks.get_mut(*event).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut event_changed = false;
        for group in groups.iter_mut() {
            let Some(hs) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = hs.len();
            hs.retain(|h| h.get("command").and_then(Value::as_str) != Some(hook_command));
            if hs.len() != before {
                event_changed = true;
            }
        }
        // Drop matcher groups whose hooks list is now empty.
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
                .unwrap_or(true)
        });
        if groups.is_empty() {
            empty_events.push((*event).to_string());
        }
        if event_changed {
            removed.push((*event).to_string());
        }
    }
    // Drop event keys with no remaining matcher groups.
    for ev in empty_events {
        hooks.remove(&ev);
    }
    removed
}

// =====================================================================
// Orchestration — read/modify/write the real files under `claude_dir`.
// =====================================================================

fn settings_path(claude_dir: &Path) -> PathBuf {
    claude_dir.join("settings.json")
}
fn hook_script_path(claude_dir: &Path) -> PathBuf {
    claude_dir.join(HOOK_SCRIPT_RELATIVE)
}
fn claude_md_path(claude_dir: &Path) -> PathBuf {
    claude_dir.join("CLAUDE.md")
}

fn read_settings(path: &Path) -> Result<Value, SetupError> {
    if !path.exists() {
        return Ok(Value::Object(Default::default()));
    }
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    Ok(serde_json::from_str(&content)?)
}

fn write_settings(path: &Path, value: &Value) -> Result<(), SetupError> {
    let mut s = serde_json::to_string_pretty(value)?;
    s.push('\n');
    fs::write(path, s)?;
    Ok(())
}

fn write_hook_script(path: &Path) -> Result<(), SetupError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, HOOK_SCRIPT_CONTENT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Wire cj into Claude Code. Idempotent: re-running on an already-installed
/// system reports `[=] already present` for every step and writes nothing.
pub fn install(claude_dir: &Path) -> Result<Report, SetupError> {
    let mut report = Report::default();

    fs::create_dir_all(claude_dir)?;

    // Hook wrapper script
    let hs = hook_script_path(claude_dir);
    let existing = fs::read_to_string(&hs).ok();
    if existing.as_deref() == Some(HOOK_SCRIPT_CONTENT) {
        report.push(Action::AlreadyPresent(format!(
            "hook script {}",
            hs.display()
        )));
    } else {
        write_hook_script(&hs)?;
        report.push(Action::Added(format!("hook script {}", hs.display())));
    }

    // settings.json
    let sp = settings_path(claude_dir);
    let mut settings = read_settings(&sp)?;
    if !settings.is_object() {
        // Recover gracefully from a stray non-object file.
        settings = Value::Object(Default::default());
    }
    let perm_added = add_permission(&mut settings, CJ_PERMISSION);
    report.push(if perm_added {
        Action::Added(format!("permission {CJ_PERMISSION}"))
    } else {
        Action::AlreadyPresent(format!("permission {CJ_PERMISSION}"))
    });
    let added_events = add_hooks(&mut settings, HOOK_COMMAND, HOOK_EVENTS);
    for ev in HOOK_EVENTS {
        if added_events.iter().any(|e| e == ev) {
            report.push(Action::Added(format!("hook for {ev}")));
        } else {
            report.push(Action::AlreadyPresent(format!("hook for {ev}")));
        }
    }
    if report.changed() {
        write_settings(&sp, &settings)?;
    }

    // CLAUDE.md instruction
    let mp = claude_md_path(claude_dir);
    let existing = fs::read_to_string(&mp).ok().unwrap_or_default();
    if existing.contains(CLAUDE_MD_MARKER) {
        report.push(Action::AlreadyPresent("CLAUDE.md instruction".to_string()));
    } else {
        let mut new_content = existing;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        if !new_content.is_empty() {
            new_content.push('\n');
        }
        new_content.push_str(CLAUDE_MD_INSTRUCTION);
        new_content.push('\n');
        fs::write(&mp, new_content)?;
        report.push(Action::Added("CLAUDE.md instruction".to_string()));
    }

    Ok(report)
}

/// Reverse `install`. The database at `~/.claude/claude-jam.db` is never
/// touched. Idempotent: re-running reports `[·] not present` for every step.
pub fn uninstall(claude_dir: &Path) -> Result<Report, SetupError> {
    let mut report = Report::default();

    // settings.json edits
    let sp = settings_path(claude_dir);
    if sp.exists() {
        let mut settings = read_settings(&sp)?;
        if !settings.is_object() {
            settings = Value::Object(Default::default());
        }

        let removed_events = remove_hooks(&mut settings, HOOK_COMMAND, HOOK_EVENTS);
        for ev in HOOK_EVENTS {
            if removed_events.iter().any(|e| e == ev) {
                report.push(Action::Removed(format!("hook for {ev}")));
            } else {
                report.push(Action::NotPresent(format!("hook for {ev}")));
            }
        }

        let perm_removed = remove_permission(&mut settings, CJ_PERMISSION);
        report.push(if perm_removed {
            Action::Removed(format!("permission {CJ_PERMISSION}"))
        } else {
            Action::NotPresent(format!("permission {CJ_PERMISSION}"))
        });

        if report.changed() {
            write_settings(&sp, &settings)?;
        }
    } else {
        for ev in HOOK_EVENTS {
            report.push(Action::NotPresent(format!("hook for {ev}")));
        }
        report.push(Action::NotPresent(format!("permission {CJ_PERMISSION}")));
    }

    // Hook script file
    let hs = hook_script_path(claude_dir);
    if hs.exists() {
        fs::remove_file(&hs)?;
        report.push(Action::Removed(format!("hook script {}", hs.display())));
    } else {
        report.push(Action::NotPresent(format!("hook script {}", hs.display())));
    }

    // CLAUDE.md instruction (line containing the marker)
    let mp = claude_md_path(claude_dir);
    if let Ok(existing) = fs::read_to_string(&mp) {
        if existing.contains(CLAUDE_MD_MARKER) {
            let cleaned: String = existing
                .lines()
                .filter(|l| !l.contains(CLAUDE_MD_MARKER))
                .collect::<Vec<_>>()
                .join("\n");
            // Restore trailing newline if original had one.
            let mut cleaned = cleaned;
            if existing.ends_with('\n') && !cleaned.ends_with('\n') {
                cleaned.push('\n');
            }
            fs::write(&mp, cleaned)?;
            report.push(Action::Removed("CLAUDE.md instruction".to_string()));
        } else {
            report.push(Action::NotPresent("CLAUDE.md instruction".to_string()));
        }
    } else {
        report.push(Action::NotPresent("CLAUDE.md instruction".to_string()));
    }

    Ok(report)
}

/// Read-only audit — reports what `install` would do without writing.
pub fn check(claude_dir: &Path) -> Result<Report, SetupError> {
    let mut report = Report::default();

    let hs = hook_script_path(claude_dir);
    if fs::read_to_string(&hs).ok().as_deref() == Some(HOOK_SCRIPT_CONTENT) {
        report.push(Action::AlreadyPresent(format!(
            "hook script {}",
            hs.display()
        )));
    } else {
        report.push(Action::NotPresent(format!(
            "hook script {} (would be created)",
            hs.display()
        )));
    }

    let settings = read_settings(&settings_path(claude_dir))?;

    let has_perm = settings
        .pointer("/permissions/allow")
        .and_then(Value::as_array)
        .map(|a| a.iter().any(|v| v.as_str() == Some(CJ_PERMISSION)))
        .unwrap_or(false);
    report.push(if has_perm {
        Action::AlreadyPresent(format!("permission {CJ_PERMISSION}"))
    } else {
        Action::NotPresent(format!("permission {CJ_PERMISSION} (would be added)"))
    });

    for ev in HOOK_EVENTS {
        let present = settings
            .pointer(&format!("/hooks/{ev}"))
            .and_then(Value::as_array)
            .map(|groups| {
                groups.iter().any(|g| {
                    g.get("hooks")
                        .and_then(Value::as_array)
                        .map(|hs| {
                            hs.iter().any(|h| {
                                h.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND)
                            })
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        report.push(if present {
            Action::AlreadyPresent(format!("hook for {ev}"))
        } else {
            Action::NotPresent(format!("hook for {ev} (would be added)"))
        });
    }

    let md_present = fs::read_to_string(claude_md_path(claude_dir))
        .map(|s| s.contains(CLAUDE_MD_MARKER))
        .unwrap_or(false);
    report.push(if md_present {
        Action::AlreadyPresent("CLAUDE.md instruction".to_string())
    } else {
        Action::NotPresent("CLAUDE.md instruction (would be added)".to_string())
    });

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_settings() -> Value {
        Value::Object(Default::default())
    }

    // ----- pure JSON helpers -----

    #[test]
    fn add_permission_creates_path_when_missing() {
        let mut s = empty_settings();
        assert!(add_permission(&mut s, "Bash(cj:*)"));
        assert_eq!(
            s.pointer("/permissions/allow")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn add_permission_is_idempotent() {
        let mut s = empty_settings();
        assert!(add_permission(&mut s, "Bash(cj:*)"));
        assert!(!add_permission(&mut s, "Bash(cj:*)"));
        assert_eq!(
            s.pointer("/permissions/allow")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn add_permission_preserves_other_permissions() {
        let mut s = serde_json::json!({
            "permissions": {"allow": ["Bash(ls:*)", "Bash(git:*)"]}
        });
        assert!(add_permission(&mut s, "Bash(cj:*)"));
        let arr = s.pointer("/permissions/allow").unwrap().as_array().unwrap();
        assert!(arr.iter().any(|v| v.as_str() == Some("Bash(ls:*)")));
        assert!(arr.iter().any(|v| v.as_str() == Some("Bash(git:*)")));
        assert!(arr.iter().any(|v| v.as_str() == Some("Bash(cj:*)")));
    }

    #[test]
    fn remove_permission_returns_false_on_missing() {
        let mut s = empty_settings();
        assert!(!remove_permission(&mut s, "Bash(cj:*)"));
    }

    #[test]
    fn remove_permission_leaves_others_alone() {
        let mut s = serde_json::json!({
            "permissions": {"allow": ["Bash(ls:*)", "Bash(cj:*)", "Bash(git:*)"]}
        });
        assert!(remove_permission(&mut s, "Bash(cj:*)"));
        let arr = s.pointer("/permissions/allow").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().all(|v| v.as_str() != Some("Bash(cj:*)")));
    }

    #[test]
    fn add_hooks_creates_catchall_when_missing() {
        let mut s = empty_settings();
        let added = add_hooks(&mut s, "h.sh", &["PreToolUse"]);
        assert_eq!(added, vec!["PreToolUse".to_string()]);
        let group = &s.pointer("/hooks/PreToolUse").unwrap().as_array().unwrap()[0];
        assert_eq!(group.get("matcher").unwrap().as_str().unwrap(), "");
        assert_eq!(
            group.get("hooks").unwrap().as_array().unwrap()[0]
                .get("command")
                .unwrap()
                .as_str()
                .unwrap(),
            "h.sh"
        );
    }

    #[test]
    fn add_hooks_uses_existing_catchall_group() {
        let mut s = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "other.sh"}]}
                ]
            }
        });
        add_hooks(&mut s, "h.sh", &["PreToolUse"]);
        let hs = s
            .pointer("/hooks/PreToolUse/0/hooks")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(hs.len(), 2);
    }

    #[test]
    fn add_hooks_is_idempotent_per_event() {
        let mut s = empty_settings();
        let first = add_hooks(&mut s, "h.sh", &["PreToolUse", "PostToolUse"]);
        let second = add_hooks(&mut s, "h.sh", &["PreToolUse", "PostToolUse"]);
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 0);
    }

    #[test]
    fn remove_hooks_strips_only_our_entry() {
        let mut s = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "", "hooks": [
                        {"type": "command", "command": "other.sh"},
                        {"type": "command", "command": "h.sh"}
                    ]}
                ]
            }
        });
        let removed = remove_hooks(&mut s, "h.sh", &["PreToolUse"]);
        assert_eq!(removed, vec!["PreToolUse".to_string()]);
        let hs = s
            .pointer("/hooks/PreToolUse/0/hooks")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].get("command").unwrap().as_str().unwrap(), "other.sh");
    }

    #[test]
    fn remove_hooks_no_op_when_missing() {
        let mut s = empty_settings();
        let removed = remove_hooks(&mut s, "h.sh", &["PreToolUse"]);
        assert!(removed.is_empty());
    }

    // ----- end-to-end on a temp directory -----

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn install_writes_all_three_artifacts() {
        let dir = tempdir();
        let report = install(dir.path()).unwrap();
        assert!(report.changed());

        // Hook script written and executable
        let hs = dir.path().join(HOOK_SCRIPT_RELATIVE);
        assert!(hs.exists());
        let content = std::fs::read_to_string(&hs).unwrap();
        assert_eq!(content, HOOK_SCRIPT_CONTENT);

        // settings.json has permission + all hooks
        let s: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert!(s
            .pointer("/permissions/allow")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(CJ_PERMISSION)));
        for ev in HOOK_EVENTS {
            assert!(
                s.pointer(&format!("/hooks/{ev}")).is_some(),
                "missing hook for {ev}"
            );
        }

        // CLAUDE.md has the instruction
        let md = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(md.contains(CLAUDE_MD_MARKER));
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tempdir();
        let first = install(dir.path()).unwrap();
        assert!(first.changed());
        let second = install(dir.path()).unwrap();
        assert!(!second.changed());
        // Every action in the second pass should be AlreadyPresent.
        for a in second.actions {
            assert!(
                matches!(a, Action::AlreadyPresent(_)),
                "expected AlreadyPresent, got {a:?}"
            );
        }
    }

    #[test]
    fn install_preserves_unrelated_settings() {
        let dir = tempdir();
        let sp = dir.path().join("settings.json");
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            &sp,
            r#"{
              "permissions": {"allow": ["Bash(git:*)"]},
              "model": "claude-sonnet-4-6",
              "hooks": {"PreToolUse": [{"matcher": "", "hooks": [{"type":"command","command":"existing.sh"}]}]}
            }"#,
        )
        .unwrap();
        install(dir.path()).unwrap();
        let s: Value = serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
        assert_eq!(
            s.pointer("/model").unwrap().as_str().unwrap(),
            "claude-sonnet-4-6"
        );
        let allow = s.pointer("/permissions/allow").unwrap().as_array().unwrap();
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(git:*)")));
        assert!(allow.iter().any(|v| v.as_str() == Some(CJ_PERMISSION)));
        let pre_hooks = s
            .pointer("/hooks/PreToolUse/0/hooks")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(pre_hooks.len(), 2);
    }

    #[test]
    fn uninstall_reverses_install() {
        let dir = tempdir();
        install(dir.path()).unwrap();
        let report = uninstall(dir.path()).unwrap();
        assert!(report.changed());

        assert!(!dir.path().join(HOOK_SCRIPT_RELATIVE).exists());

        let s: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        let allow = s.pointer("/permissions/allow").and_then(Value::as_array);
        assert!(allow
            .map(|a| a.iter().all(|v| v.as_str() != Some(CJ_PERMISSION)))
            .unwrap_or(true));
        for ev in HOOK_EVENTS {
            let groups = s.pointer(&format!("/hooks/{ev}"));
            if let Some(groups) = groups.and_then(Value::as_array) {
                for g in groups {
                    if let Some(hs) = g.get("hooks").and_then(Value::as_array) {
                        for h in hs {
                            assert_ne!(
                                h.get("command").and_then(Value::as_str),
                                Some(HOOK_COMMAND),
                                "hook still registered for {ev}"
                            );
                        }
                    }
                }
            }
        }

        let md = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(!md.contains(CLAUDE_MD_MARKER));
    }

    #[test]
    fn uninstall_no_op_on_clean_dir() {
        let dir = tempdir();
        let report = uninstall(dir.path()).unwrap();
        assert!(!report.changed());
        for a in report.actions {
            assert!(matches!(a, Action::NotPresent(_)), "got {a:?}");
        }
    }

    #[test]
    fn check_reports_not_present_on_fresh_dir() {
        let dir = tempdir();
        let report = check(dir.path()).unwrap();
        // Nothing should be marked Added/Removed by `check` (it's read-only).
        for a in &report.actions {
            assert!(matches!(
                a,
                Action::AlreadyPresent(_) | Action::NotPresent(_)
            ));
        }
        // On a fresh dir, everything should be NotPresent.
        assert!(report
            .actions
            .iter()
            .all(|a| matches!(a, Action::NotPresent(_))));
    }

    #[test]
    fn check_reports_all_present_after_install() {
        let dir = tempdir();
        install(dir.path()).unwrap();
        let report = check(dir.path()).unwrap();
        assert!(report
            .actions
            .iter()
            .all(|a| matches!(a, Action::AlreadyPresent(_))));
    }

    #[test]
    fn install_recovers_from_empty_or_broken_settings_file() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("settings.json"), "").unwrap();
        install(dir.path()).unwrap();
        let s: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert!(s.is_object());
    }
}
