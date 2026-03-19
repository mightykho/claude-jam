#!/bin/bash
set -e

# Claude Jam uninstaller
# Removes binary, hooks, settings entries, and CLAUDE.md instructions.

CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"
SETTINGS_FILE="$CLAUDE_DIR/settings.json"
HOOK_SCRIPT="$HOOKS_DIR/claude-jam.sh"
BIN_DIR="$HOME/bin"
BINARY_NAME="cj"
DB_FILE="$CLAUDE_DIR/claude-jam.db"

echo "Uninstalling Claude Jam..."
echo ""

# --- 1. Remove binary ---

if [ -f "$BIN_DIR/$BINARY_NAME" ] || [ -L "$BIN_DIR/$BINARY_NAME" ]; then
    rm "$BIN_DIR/$BINARY_NAME"
    echo "[ok] Removed $BIN_DIR/$BINARY_NAME"
else
    echo "[ok] Binary not found, skipping"
fi

# --- 2. Remove hook script ---

if [ -f "$HOOK_SCRIPT" ]; then
    rm "$HOOK_SCRIPT"
    echo "[ok] Removed $HOOK_SCRIPT"
else
    echo "[ok] Hook script not found, skipping"
fi

# --- 3. Remove from Claude Code settings ---

if [ -f "$SETTINGS_FILE" ]; then
    python3 << 'PYTHON'
import json, os

settings_file = os.path.expanduser("~/.claude/settings.json")

with open(settings_file, "r") as f:
    settings = json.load(f)

changed = False

# Remove cj permission
perms = settings.get("permissions", {}).get("allow", [])
if "Bash(cj:*)" in perms:
    perms.remove("Bash(cj:*)")
    changed = True
    print("[ok] Removed 'Bash(cj:*)' permission")

# Remove claude-jam hooks from all events
hook_command = "~/.claude/hooks/claude-jam.sh"
hooks = settings.get("hooks", {})
for event, matcher_groups in hooks.items():
    for mg in matcher_groups:
        before = len(mg.get("hooks", []))
        mg["hooks"] = [h for h in mg.get("hooks", []) if h.get("command") != hook_command]
        if len(mg["hooks"]) < before:
            changed = True

    # Clean up empty matcher groups
    hooks[event] = [mg for mg in matcher_groups if mg.get("hooks")]

# Clean up empty hook events
settings["hooks"] = {k: v for k, v in hooks.items() if v}

if changed:
    with open(settings_file, "w") as f:
        json.dump(settings, f, indent=2)
        f.write("\n")
    print("[ok] Cleaned up settings.json")
else:
    print("[ok] No settings to clean up")

PYTHON
else
    echo "[ok] No settings file found, skipping"
fi

# --- 4. Remove CLAUDE.md instruction ---

CLAUDE_MD="$CLAUDE_DIR/CLAUDE.md"
if [ -f "$CLAUDE_MD" ] && grep -q "Claude Jam" "$CLAUDE_MD"; then
    sed -i '' '/Claude Jam/d' "$CLAUDE_MD"
    # Remove trailing blank lines
    sed -i '' -e :a -e '/^\n*$/{$d;N;ba' -e '}' "$CLAUDE_MD"
    echo "[ok] Removed cj instructions from CLAUDE.md"
else
    echo "[ok] No CLAUDE.md instructions to remove"
fi

# --- 5. Database ---

if [ -f "$DB_FILE" ]; then
    echo ""
    echo "Database at $DB_FILE was kept (contains session history)."
    echo "To remove it: rm $DB_FILE"
else
    echo "[ok] No database found"
fi

echo ""
echo "Claude Jam uninstalled."
