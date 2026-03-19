#!/bin/bash
set -e

# Claude Jam installer
# Builds the binary, sets up hooks, and configures Claude Code integration.

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"
SETTINGS_FILE="$CLAUDE_DIR/settings.json"
HOOK_SCRIPT="$HOOKS_DIR/claude-jam.sh"
BIN_DIR="$HOME/bin"
BINARY_NAME="cj"

echo "Installing Claude Jam..."
echo ""

# --- 1. Check dependencies ---

if ! command -v cargo &>/dev/null; then
    echo "Rust toolchain not found."
    echo "Install it with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi
echo "[ok] Rust toolchain found"

if ! command -v tmux &>/dev/null; then
    echo "[warn] tmux not found — session switching will not work"
else
    echo "[ok] tmux found"
fi

if ! command -v claude &>/dev/null; then
    echo "[warn] claude CLI not found — hooks won't fire until it's installed"
else
    echo "[ok] claude CLI found"
fi

# --- 2. Build release binary ---

echo ""
echo "Building release binary..."
cd "$REPO_DIR"
cargo build --release --quiet
echo "[ok] Built target/release/claude-jam"

# --- 3. Install binary ---

mkdir -p "$BIN_DIR"

# Remove existing symlink or binary
if [ -L "$BIN_DIR/$BINARY_NAME" ] || [ -f "$BIN_DIR/$BINARY_NAME" ]; then
    rm "$BIN_DIR/$BINARY_NAME"
fi

ln -s "$REPO_DIR/target/release/claude-jam" "$BIN_DIR/$BINARY_NAME"
echo "[ok] Linked $BIN_DIR/$BINARY_NAME -> target/release/claude-jam"

# Verify it's on PATH
if ! command -v "$BINARY_NAME" &>/dev/null; then
    echo "[warn] $BIN_DIR is not on your PATH. Add it:"
    echo "       export PATH=\"$BIN_DIR:\$PATH\""
fi

# --- 4. Set up hook script ---

mkdir -p "$HOOKS_DIR"

cat > "$HOOK_SCRIPT" << 'HOOK'
#!/bin/bash
exec cj hook
HOOK
chmod +x "$HOOK_SCRIPT"
echo "[ok] Created hook script at $HOOK_SCRIPT"

# --- 5. Configure Claude Code settings ---

mkdir -p "$CLAUDE_DIR"

if [ ! -f "$SETTINGS_FILE" ]; then
    echo '{}' > "$SETTINGS_FILE"
fi

# Use a Python script for reliable JSON manipulation (available on macOS and most Linux)
python3 << 'PYTHON'
import json, sys, os

settings_file = os.path.expanduser("~/.claude/settings.json")

with open(settings_file, "r") as f:
    settings = json.load(f)

# Ensure permissions structure exists
if "permissions" not in settings:
    settings["permissions"] = {}
if "allow" not in settings["permissions"]:
    settings["permissions"]["allow"] = []

# Add cj bash permission if not present
cj_perm = "Bash(cj:*)"
if cj_perm not in settings["permissions"]["allow"]:
    settings["permissions"]["allow"].append(cj_perm)
    print("[ok] Added 'Bash(cj:*)' to allowed permissions")
else:
    print("[ok] 'Bash(cj:*)' permission already present")

# Ensure hooks structure exists
if "hooks" not in settings:
    settings["hooks"] = {}

hook_command = "~/.claude/hooks/claude-jam.sh"
hook_entry = {"type": "command", "command": hook_command}

# Hook events that cj needs to listen to
hook_events = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "SessionEnd",
]

added_hooks = []
for event in hook_events:
    if event not in settings["hooks"]:
        settings["hooks"][event] = []

    # Check if claude-jam hook already registered for this event
    already_present = False
    for matcher_group in settings["hooks"][event]:
        for hook in matcher_group.get("hooks", []):
            if hook.get("command") == hook_command:
                already_present = True
                break

    if not already_present:
        # Find or create the catch-all matcher group (empty matcher = match all)
        catchall = None
        for mg in settings["hooks"][event]:
            if mg.get("matcher", "") == "":
                catchall = mg
                break
        if catchall is None:
            catchall = {"matcher": "", "hooks": []}
            settings["hooks"][event].append(catchall)
        catchall["hooks"].append(hook_entry)
        added_hooks.append(event)

if added_hooks:
    print(f"[ok] Registered hooks for: {', '.join(added_hooks)}")
else:
    print("[ok] All hooks already registered")

with open(settings_file, "w") as f:
    json.dump(settings, f, indent=2)
    f.write("\n")

PYTHON

# --- 6. Add CLAUDE.md instruction ---

CLAUDE_MD="$CLAUDE_DIR/CLAUDE.md"
CJ_INSTRUCTION='- Claude Jam (`cj`) tracks session context. When you establish the main goal of a session (after understanding the task, reading a ticket, etc.), run: `cj topic "concise description of the goal"`. When you complete a significant step or milestone, run: `cj milestone "what was accomplished"`. Keep descriptions short and informative.'

if [ -f "$CLAUDE_MD" ]; then
    if grep -q "Claude Jam" "$CLAUDE_MD"; then
        echo "[ok] CLAUDE.md already contains cj instructions"
    else
        echo "" >> "$CLAUDE_MD"
        echo "$CJ_INSTRUCTION" >> "$CLAUDE_MD"
        echo "[ok] Added cj instructions to CLAUDE.md"
    fi
else
    echo "$CJ_INSTRUCTION" > "$CLAUDE_MD"
    echo "[ok] Created CLAUDE.md with cj instructions"
fi

# --- 7. Initialize database ---

"$BIN_DIR/$BINARY_NAME" hook < /dev/null 2>/dev/null || true
echo "[ok] Database initialized at ~/.claude/claude-jam.db"

# --- Done ---

echo ""
echo "Claude Jam installed successfully!"
echo ""
echo "Usage:"
"$BIN_DIR/$BINARY_NAME" -h
