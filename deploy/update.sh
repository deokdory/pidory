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
echo "[1/4] Pulling latest changes..."
OLD_HEAD=$(git rev-parse HEAD)
git pull --ff-only || { echo "ERROR: Fast-forward pull failed. Resolve manually."; exit 1; }

# 3. Check for systemd unit drift
echo "[2/4] Checking systemd unit drift..."

if [ "$OLD_HEAD" = "$(git rev-parse HEAD)" ]; then
    echo "  ✅ No new commits — nothing to check."
elif git diff --name-only "$OLD_HEAD"..HEAD 2>/dev/null | grep -qE '^deploy/(pidory(-delayed-restart|-dev)?\.service|com\.pidory\.bot\.plist)$'; then
    echo "  ❌ systemd unit files changed in this pull."
    echo "     update.sh does NOT update systemd units."
    echo "     Run 'bash deploy/install.sh' instead to apply unit changes."
    exit 1
else
    echo "  ✅ No systemd unit changes detected."
fi

# 4. Build
echo "[3/4] Building release binary..."
cargo build --release
cargo build --bin pidory-migrate --features migrate --release

# Install updated pidory-migrate binary (required for v0.7.1+ migrations)
USER_NAME="${SUDO_USER:-$USER}"
sudo install -o "$USER_NAME" -m 0755 \
    "$PROJECT_DIR/target/release/pidory-migrate" \
    /usr/local/bin/pidory-migrate
echo "  Installed pidory-migrate → /usr/local/bin/pidory-migrate"

# 5. Sync skills
echo "  Syncing skills..."
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

# 6. Pre-flight migration verification
echo "[4/4] Pre-flight migration verification..."

DB_ENV_FILE="/etc/pidory/db.env"
if [ ! -f "$DB_ENV_FILE" ]; then
    echo "  ⚠️  $DB_ENV_FILE not found — skipping migration check."
    if [ -t 0 ] && [ -t 1 ]; then
        read -p "  Press Enter to acknowledge..." _
    fi
else
    if bash "$PROJECT_DIR/scripts/pidory-migrate.sh"; then
        echo "  ✅ Migration verified."
    else
        RC=$?
        echo "  ❌ Migration verification failed (exit $RC)."
        exit "$RC"
    fi
fi

# 7. Restart guidance
echo ""
echo "=== Update complete ==="
if [ "$OS" = "Darwin" ]; then
    echo "Restart: launchctl kickstart -k gui/$(id -u)/com.pidory.bot"
else
    echo "Restart: sudo systemctl restart pidory"
fi
