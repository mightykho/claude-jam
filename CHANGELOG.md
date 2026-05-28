# Changelog

All notable changes are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] — 2026-05-28

First public release.

### Changed
- Refactored the monolithic 1964-line `src/main.rs` into a lib + bin split. `lib.rs` exposes `db`, `hook`, `models`, `time`, and `tmux`; the binary owns `commands`, `tui`, and arg dispatch. The biggest file is now 369 lines. The 44 tests run as 13 (lib unit) + 12 (bin unit) + 19 (integration against in-memory SQLite).

### Added
- `rust-toolchain.toml` pins the Rust toolchain to `1.95` so local `cargo clippy` matches CI.

### Fixed
- Three `collapsible_match` clippy errors that landed in Rust 1.95 and broke the CI build for the v0.1.0 tag.

## [0.1.0] — 2026-05-28

Initial release.

### Added
- TUI dashboard for browsing concurrent Claude Code sessions, with one-keystroke switching to the underlying tmux session.
- Lifecycle-event hook (`cj hook`) that derives status (`working`, `waiting`, `idle`, `pending`, `offline`) from the Claude Code event and writes it to a shared SQLite database at `~/.claude/claude-jam.db`.
- Context-window tracking. The hook parses the session's transcript on every event, computes `input + cache + output` tokens, and renders an 8-cell mini progress bar in the TUI (green under 60%, yellow 60–80%, red 80%+). Defaults to 200K and auto-bumps to 1M once observed usage crosses 200K. Exposed as `cj context` for tmux status lines.
- `cj topic` and `cj milestone` for session annotations, rendered inline in the dashboard.
- `cj init` pre-registers a tmux session with a topic; the hook migrates the topic (and any milestones) onto the real session row as soon as Claude fires its next event for that tmux session.
- `cj import` bulk-registers every active tmux session that cj isn't already tracking.
- `cj remove` drops all sessions matching a tmux name.
- Delete confirmation popup (`d` → `Y`/`N`/`Esc`) so accidental keystrokes don't lose work.
- Layout flags: `-b` / `--borderless` drops the title bar, `-v` / `--vertical` splits each entry into title + detail lines.
- `install.sh --dev` symlinks `~/bin/cj` to the build output so `cargo build` alone refreshes the running binary.
- 44 unit + integration tests covering pure helpers, transcript parsing, and the DB layer against in-memory SQLite.
- GitHub Actions CI matrix on Ubuntu + macOS running `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test`.

[Unreleased]: https://github.com/mightykho/claude-jam/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/mightykho/claude-jam/releases/tag/v0.1.1
[0.1.0]: https://github.com/mightykho/claude-jam/releases/tag/v0.1.0
