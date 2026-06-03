#!/bin/bash
set -e

# Claude Jam uninstaller for the git-clone path.
#
# Reverses `cj setup` (hook script, settings.json entries, CLAUDE.md line)
# via the binary itself, then removes the binary. The SQLite database is
# preserved so session history survives accidental teardowns.

CLAUDE_DIR="$HOME/.claude"
DB_FILE="$CLAUDE_DIR/claude-jam.db"
BIN_DIR="$HOME/bin"
BINARY_NAME="cj"

echo "Uninstalling Claude Jam..."
echo ""

# --- 1. Reverse cj setup before removing the binary ---

if command -v "$BINARY_NAME" &>/dev/null; then
    "$BINARY_NAME" teardown
    echo ""
elif [ -x "$BIN_DIR/$BINARY_NAME" ]; then
    "$BIN_DIR/$BINARY_NAME" teardown
    echo ""
else
    echo "[warn] cj binary not on PATH; skipping teardown step."
    echo "       If hooks/settings entries linger, install cj again and run \`cj teardown\`."
fi

# --- 2. Remove binary ---

if [ -L "$BIN_DIR/$BINARY_NAME" ] || [ -f "$BIN_DIR/$BINARY_NAME" ]; then
    rm "$BIN_DIR/$BINARY_NAME"
    echo "[ok] Removed $BIN_DIR/$BINARY_NAME"
else
    echo "[ok] Binary not found at $BIN_DIR/$BINARY_NAME, skipping"
fi

# --- 3. Database stays (intentional) ---

if [ -f "$DB_FILE" ]; then
    echo ""
    echo "Database at $DB_FILE was kept (contains session history)."
    echo "To remove it: rm $DB_FILE"
fi

echo ""
echo "Claude Jam uninstalled."
