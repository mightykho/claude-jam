#!/bin/bash
set -e

# Claude Jam installer
# Sets up the binary, hooks, and Claude Code integration.
#
# Usage:
#   ./install.sh         End-user install (copies the binary into ~/bin)
#   ./install.sh --dev   Developer install (symlinks ~/bin/cj to the build
#                        output so `cargo build --release` alone refreshes
#                        the running binary)

DEV_MODE=0
for arg in "$@"; do
    case "$arg" in
        --dev) DEV_MODE=1 ;;
        -h|--help)
            sed -n '4,11p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
    esac
done

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"
SETTINGS_FILE="$CLAUDE_DIR/settings.json"
HOOK_SCRIPT="$HOOKS_DIR/claude-jam.sh"
BIN_DIR="$HOME/bin"
BINARY_NAME="cj"
PREBUILT="$REPO_DIR/target/release/cj"

echo "Installing Claude Jam..."
echo ""

# --- 1. Get binary ---

if [ -f "$PREBUILT" ]; then
    echo "[ok] Found prebuilt binary"
elif command -v cargo &>/dev/null; then
    echo "No prebuilt binary found, building from source..."
    cd "$REPO_DIR"
    cargo build --release --quiet
    echo "[ok] Built from source"
else
    echo "Error: No prebuilt binary and Rust toolchain not found."
    echo "Either build first with 'cargo build --release' or download a release binary."
    exit 1
fi

# --- 2. Install binary ---

mkdir -p "$BIN_DIR"

if [ -L "$BIN_DIR/$BINARY_NAME" ] || [ -f "$BIN_DIR/$BINARY_NAME" ]; then
    rm "$BIN_DIR/$BINARY_NAME"
fi

if [ "$DEV_MODE" = "1" ]; then
    ln -s "$PREBUILT" "$BIN_DIR/$BINARY_NAME"
    echo "[ok] Symlinked $BIN_DIR/$BINARY_NAME -> $PREBUILT (dev mode)"
else
    cp "$PREBUILT" "$BIN_DIR/$BINARY_NAME"
    chmod +x "$BIN_DIR/$BINARY_NAME"
    echo "[ok] Installed $BIN_DIR/$BINARY_NAME"
fi

if ! command -v "$BINARY_NAME" &>/dev/null; then
    echo "[warn] $BIN_DIR is not on your PATH. Add it:"
    echo "       export PATH=\"$BIN_DIR:\$PATH\""
fi

# --- 3. Set up hook script ---

mkdir -p "$HOOKS_DIR"

cat > "$HOOK_SCRIPT" << 'HOOK'
#!/bin/bash
exec cj hook
HOOK
chmod +x "$HOOK_SCRIPT"
echo "[ok] Created hook at $HOOK_SCRIPT"

# --- 4. Configure Claude Code settings ---

mkdir -p "$CLAUDE_DIR"

if [ ! -f "$SETTINGS_FILE" ]; then
    echo '{}' > "$SETTINGS_FILE"
fi

python3 << 'PYTHON'
import json, os

settings_file = os.path.expanduser("~/.claude/settings.json")

with open(settings_file, "r") as f:
    settings = json.load(f)

# Ensure permissions structure
if "permissions" not in settings:
    settings["permissions"] = {}
if "allow" not in settings["permissions"]:
    settings["permissions"]["allow"] = []

# Add cj bash permission
cj_perm = "Bash(cj:*)"
if cj_perm not in settings["permissions"]["allow"]:
    settings["permissions"]["allow"].append(cj_perm)
    print("[ok] Added 'Bash(cj:*)' to allowed permissions")
else:
    print("[ok] 'Bash(cj:*)' permission already present")

# Ensure hooks structure
if "hooks" not in settings:
    settings["hooks"] = {}

hook_command = "~/.claude/hooks/claude-jam.sh"
hook_entry = {"type": "command", "command": hook_command}

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

    already_present = False
    for matcher_group in settings["hooks"][event]:
        for hook in matcher_group.get("hooks", []):
            if hook.get("command") == hook_command:
                already_present = True
                break

    if not already_present:
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

# --- 5. Add CLAUDE.md instruction ---

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

# --- 6. Initialize database ---

"$BIN_DIR/$BINARY_NAME" hook < /dev/null 2>/dev/null || true
echo "[ok] Database initialized at ~/.claude/claude-jam.db"

# --- Done ---

echo ""
echo "Claude Jam installed successfully!"
echo ""
"$BIN_DIR/$BINARY_NAME" -h
