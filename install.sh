#!/bin/bash
set -e

# Claude Jam installer for the git-clone path.
#
# Builds (or finds) the binary, drops it into ~/bin/cj, then delegates the
# Claude Code wiring to `cj setup` so the same logic runs for every install
# channel (brew, cargo install, prebuilt tarball, manual clone).
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
            sed -n '4,14p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
    esac
done

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
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

# --- 2. Install binary into ~/bin ---

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

# --- 3. Wire into Claude Code via `cj setup` ---

echo ""
"$BIN_DIR/$BINARY_NAME" setup

# --- 4. Initialize database ---

"$BIN_DIR/$BINARY_NAME" hook < /dev/null 2>/dev/null || true

echo ""
echo "Claude Jam installed successfully!"
