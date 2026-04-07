#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
OS="$(uname -s)"

echo "=== pidory update ($OS) ==="

# 1. Dirty check
cd "$PROJECT_DIR"
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "ERROR: Uncommitted changes detected. Commit or stash before updating."
    exit 1
fi

# 2. Git pull
echo "[1/3] Pulling latest changes..."
git pull --ff-only || { echo "ERROR: Fast-forward pull failed. Resolve manually."; exit 1; }

# 3. Build
echo "[2/3] Building release binary..."
cargo build --release

# 4. Sync skills
echo "[3/3] Syncing skills..."
SKILLS_TARGET="$HOME/.claude/skills"
if [ -d "$PROJECT_DIR/skills" ]; then
    mkdir -p "$SKILLS_TARGET"
    shopt -s nullglob dotglob
    for skill_dir in "$PROJECT_DIR/skills"/*/; do
        skill_name="$(basename "$skill_dir")"
        mkdir -p "$SKILLS_TARGET/$skill_name"
        cp -r "$skill_dir"* "$SKILLS_TARGET/$skill_name/"
        echo "  Synced: $skill_name"
    done
    shopt -u nullglob dotglob
fi

# 5. Restart guidance
echo ""
echo "=== Update complete ==="
if [ "$OS" = "Darwin" ]; then
    echo "Restart: launchctl kickstart -k gui/$(id -u)/com.pidory.bot"
else
    echo "Restart: sudo systemctl restart pidory"
fi
