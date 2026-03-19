# Claude Jam

A TUI dashboard for monitoring and switching between multiple concurrent Claude Code sessions.

Claude Jam hooks into Claude Code's lifecycle events to track what each session is doing in real time — which tool it's running, whether it's waiting for input, or if it's done. Sessions are displayed in a compact list with emoji status indicators, topic descriptions, and milestone history.

## How it works

A lightweight hook script pipes Claude Code events (session start, tool use, notifications, etc.) into a shared SQLite database. The TUI reads from this database and refreshes every second. Each session shows:

- Status emoji: 🔨 working, 🔔 waiting for input, ✅ done, 💤 stale, ⏳ pending
- tmux session name and relative timestamp
- Current tool and detail (e.g. `Read src/main.rs`)
- Topic and milestone history (set by Claude via `cj topic` / `cj milestone`)

Press Enter on any session to switch to its tmux session. Press `o` to expand milestone history.

## Install

```bash
git clone <repo-url> && cd claude-jam
cargo build --release
./install.sh
```

If Rust is installed, `install.sh` will build automatically if no prebuilt binary exists. The installer:

- Copies the binary to `~/bin/cj`
- Creates the hook script at `~/.claude/hooks/claude-jam.sh`
- Registers hooks for all Claude Code lifecycle events in `~/.claude/settings.json`
- Adds `Bash(cj:*)` to allowed permissions so Claude can run `cj` commands
- Adds instructions to `~/.claude/CLAUDE.md` so Claude knows to report topics and milestones

## Uninstall

```bash
./uninstall.sh
```

Removes the binary, hook, settings entries, and CLAUDE.md instructions. The SQLite database at `~/.claude/claude-jam.db` is kept — delete it manually if you want a clean slate.

## Usage

```
cj                          Launch TUI dashboard
cj -q                       Launch TUI, quit after selecting a session
cj init [-s name] <topic>   Pre-register session with topic before Claude starts
cj topic <text>             Set topic for the current session
cj milestone <text>         Add milestone to the current session
cj remove <tmux-session>    Remove all sessions for a tmux session
cj hook                     Process hook event from stdin (used internally)
cj -h                       Show help
```

## TUI keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate sessions |
| `1`-`9` | Jump to session by number |
| `Ctrl-a`..`Ctrl-z` | Jump to session by letter (after 9) |
| `Enter` | Switch to session's tmux session |
| `o` | Expand/collapse milestone history |
| `d` | Delete session |
| `q` | Quit |

## tmux integration (optional)

Bind `cj` to a tmux key for quick access. Add to your `~/.tmux.conf`:

```tmux
# Leader-w opens Claude Jam in a popup (auto-closes on session select)
bind w display-popup -E "cj -q"
```

Reload with `tmux source-file ~/.tmux.conf`. Now `<prefix> w` opens a popup showing all Claude sessions — select one and it switches immediately.

## How Claude reports context

The installer adds instructions to `~/.claude/CLAUDE.md` that tell Claude to run:

- `cj topic "description"` when it understands the main goal of a session
- `cj milestone "what was accomplished"` after completing a significant step

These show up in the dashboard under each session. Topics appear in bold, milestones with a ⚑ marker and timestamp. Press `o` to see the full milestone history for a session.
