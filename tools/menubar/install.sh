#!/usr/bin/env bash
#
# pidory menubar installer (macOS).
#
# Sets up a Python venv with rumps, renders a launchd plist with absolute
# paths for the current user, and loads it. Idempotent — safe to re-run.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
TEMPLATE="$SCRIPT_DIR/com.pidory.menubar.plist.template"
MENUBAR_PY="$SCRIPT_DIR/menubar.py"

VENV_DIR="$HOME/.pidory/menubar-venv"
PLIST_DEST="$HOME/Library/LaunchAgents/com.pidory.menubar.plist"
LABEL="com.pidory.menubar"

echo "=== pidory menubar installer ==="

# 1. Platform check
if [ "$(uname -s)" != "Darwin" ]; then
    echo "ERROR: this menubar app only supports macOS." >&2
    exit 1
fi

# 2. Sanity-check repo layout
if [ ! -f "$MENUBAR_PY" ] || [ ! -f "$TEMPLATE" ]; then
    echo "ERROR: menubar.py or plist template missing — wrong script location?" >&2
    exit 1
fi

# 3. Detect Homebrew bin path (Apple Silicon vs Intel)
if [ -d "/opt/homebrew/bin" ]; then
    HOMEBREW_BIN="/opt/homebrew/bin"
elif [ -d "/usr/local/bin" ]; then
    HOMEBREW_BIN="/usr/local/bin"
else
    HOMEBREW_BIN="/usr/local/bin"
fi
echo "Homebrew bin: $HOMEBREW_BIN"

# 4. Find a usable python3 (>= 3.9)
PYTHON_BIN="$(command -v python3 || true)"
if [ -z "$PYTHON_BIN" ]; then
    echo "ERROR: python3 not found in PATH. Install with 'brew install python'." >&2
    exit 1
fi
PY_VERSION="$("$PYTHON_BIN" -c 'import sys; print("%d.%d" % sys.version_info[:2])')"
echo "Python: $PYTHON_BIN ($PY_VERSION)"

# 5. Create / refresh venv
echo "[1/4] Creating venv at $VENV_DIR"
mkdir -p "$(dirname "$VENV_DIR")"
if [ ! -d "$VENV_DIR" ]; then
    "$PYTHON_BIN" -m venv "$VENV_DIR"
fi
VENV_PYTHON="$VENV_DIR/bin/python"

# 6. Install rumps
echo "[2/4] Installing rumps into venv"
"$VENV_PYTHON" -m pip install --quiet --upgrade pip
"$VENV_PYTHON" -m pip install --quiet rumps

# 7. Render plist from template
echo "[3/4] Rendering launchd plist → $PLIST_DEST"
mkdir -p "$(dirname "$PLIST_DEST")"
sed \
    -e "s|{{VENV_PYTHON}}|$VENV_PYTHON|g" \
    -e "s|{{MENUBAR_PY}}|$MENUBAR_PY|g" \
    -e "s|{{HOMEBREW_BIN}}|$HOMEBREW_BIN|g" \
    -e "s|{{HOME}}|$HOME|g" \
    "$TEMPLATE" > "$PLIST_DEST"

# 8. (Re)load the agent
echo "[4/4] Loading launchd agent"
if launchctl list | grep -q "$LABEL"; then
    launchctl unload "$PLIST_DEST" 2>/dev/null || true
fi
launchctl load "$PLIST_DEST"

# 9. Verify
sleep 1
if launchctl list | grep -q "$LABEL"; then
    echo ""
    echo "=== Installed ==="
    echo "Look for the pidory icon in your menu bar (top right)."
    echo "Logs: ~/.pidory/menubar.log, ~/.pidory/menubar.stderr.log"
    echo "Uninstall: $SCRIPT_DIR/uninstall.sh"
else
    echo "WARN: agent did not appear in launchctl list. Check ~/.pidory/menubar.stderr.log" >&2
    exit 1
fi
