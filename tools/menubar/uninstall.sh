#!/usr/bin/env bash
#
# pidory menubar uninstaller (macOS).
#
# Removes the launchd agent and (optionally) the venv.
#
set -euo pipefail

VENV_DIR="$HOME/.pidory/menubar-venv"
PLIST_DEST="$HOME/Library/LaunchAgents/com.pidory.menubar.plist"
LABEL="com.pidory.menubar"

echo "=== pidory menubar uninstaller ==="

# 1. Unload agent
if launchctl list | grep -q "$LABEL"; then
    echo "[1/3] Unloading $LABEL"
    launchctl unload "$PLIST_DEST" 2>/dev/null || true
else
    echo "[1/3] Agent not loaded, skipping"
fi

# 2. Remove plist
if [ -f "$PLIST_DEST" ]; then
    echo "[2/3] Removing $PLIST_DEST"
    rm -f "$PLIST_DEST"
else
    echo "[2/3] Plist not present, skipping"
fi

# 3. Optionally remove venv
if [ -d "$VENV_DIR" ]; then
    read -p "[3/3] Remove venv at $VENV_DIR? [y/N] " answer
    case "$answer" in
        [yY]|[yY][eE][sS])
            rm -rf "$VENV_DIR"
            echo "  removed"
            ;;
        *)
            echo "  kept"
            ;;
    esac
else
    echo "[3/3] Venv not present, skipping"
fi

echo ""
echo "=== Uninstalled ==="
echo "Logs at ~/.pidory/menubar*.log are kept. Remove manually if you like."
