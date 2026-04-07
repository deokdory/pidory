#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
USER_NAME="$(whoami)"
HOME_DIR="$HOME"
OS="$(uname -s)"

echo "=== pidory deployment ($OS) ==="

# 1. Build
echo "[1/5] Building release binary..."
cd "$PROJECT_DIR"
cargo build --release

# 2. Check .env
echo "[2/5] Checking environment..."
if [ ! -f "$PROJECT_DIR/.env" ]; then
    echo "WARNING: .env not found."
    echo "Create it with: echo 'PIDORY_DISCORD_TOKEN=your_token' > $PROJECT_DIR/.env"
fi

# 3. Copy config if not exists + detect claude CLI path
if [ ! -f "$PROJECT_DIR/config.toml" ]; then
    echo "[3/5] Creating config.toml from example..."
    cp "$PROJECT_DIR/config.toml.example" "$PROJECT_DIR/config.toml"
    echo "IMPORTANT: Edit config.toml with your Discord guild_id and owner_id"
else
    echo "[3/5] config.toml already exists, skipping"
fi

# Detect claude CLI absolute path and inject into config.toml
CLAUDE_BIN="$(which claude 2>/dev/null || true)"
if [ -n "$CLAUDE_BIN" ]; then
    echo "     Detected claude CLI: $CLAUDE_BIN"
    sed -i.bak "s|^binary_path = .*|binary_path = \"$CLAUDE_BIN\"|" "$PROJECT_DIR/config.toml"
    rm -f "$PROJECT_DIR/config.toml.bak"
else
    echo "WARNING: claude CLI not found in PATH. Set binary_path in config.toml manually."
fi

# 4. Install service
echo "[4/5] Installing service..."

if [ "$OS" = "Darwin" ]; then
    # macOS — launchd
    PLIST_DIR="$HOME_DIR/Library/LaunchAgents"
    PLIST_NAME="com.pidory.bot.plist"
    LOG_DIR="$HOME_DIR/.pidory"

    mkdir -p "$PLIST_DIR" "$LOG_DIR"

    # .env에서 토큰 읽기
    DISCORD_TOKEN=""
    if [ -f "$PROJECT_DIR/.env" ]; then
        DISCORD_TOKEN=$(grep -oP 'PIDORY_DISCORD_TOKEN=\K.*' "$PROJECT_DIR/.env" 2>/dev/null || \
                        sed -n 's/^PIDORY_DISCORD_TOKEN=//p' "$PROJECT_DIR/.env")
    fi

    if [ -z "$DISCORD_TOKEN" ]; then
        echo "WARNING: Could not read PIDORY_DISCORD_TOKEN from .env"
        echo "You will need to edit $PLIST_DIR/$PLIST_NAME manually"
        DISCORD_TOKEN="YOUR_TOKEN_HERE"
    fi

    sed -e "s|__PROJECT_DIR__|$PROJECT_DIR|g" \
        -e "s|__HOME_DIR__|$HOME_DIR|g" \
        -e "s|__DISCORD_TOKEN__|$DISCORD_TOKEN|g" \
        "$SCRIPT_DIR/com.pidory.bot.plist" > "$PLIST_DIR/$PLIST_NAME"

    # 기존 서비스 언로드 (실패해도 무시)
    launchctl bootout "gui/$(id -u)/$PLIST_NAME" 2>/dev/null || true

    echo ""
    echo "=== Done ==="
    echo "Start:   launchctl load $PLIST_DIR/$PLIST_NAME"
    echo "Stop:    launchctl unload $PLIST_DIR/$PLIST_NAME"
    echo "Logs:    tail -f $LOG_DIR/stderr.log"

else
    # Linux — systemd
    sed -e "s|__USER__|$USER_NAME|g" \
        -e "s|__PROJECT_DIR__|$PROJECT_DIR|g" \
        -e "s|__HOME_DIR__|$HOME_DIR|g" \
        "$SCRIPT_DIR/pidory.service" | sudo tee /etc/systemd/system/pidory.service > /dev/null
    sudo systemctl daemon-reload
    sudo systemctl enable pidory

    echo ""
    echo "=== Done ==="
    echo "Start:   sudo systemctl start pidory"
    echo "Status:  sudo systemctl status pidory"
    echo "Logs:    journalctl -u pidory -f"
fi

# 5. Install skills
echo "[5/5] Installing skills..."
SKILLS_TARGET="$HOME/.claude/skills"
if [ -d "$PROJECT_DIR/skills" ]; then
    mkdir -p "$SKILLS_TARGET"
    for skill_dir in "$PROJECT_DIR/skills"/*/; do
        skill_name="$(basename "$skill_dir")"
        mkdir -p "$SKILLS_TARGET/$skill_name"
        cp -r "$skill_dir"* "$SKILLS_TARGET/$skill_name/"
        echo "  Installed: $skill_name"
    done
else
    echo "  No skills directory found, skipping"
fi

echo ""
echo "Don't forget to:"
echo "  1. Edit config.toml with your Discord guild_id and owner_id"
echo "  2. Set Discord token in .env: echo 'PIDORY_DISCORD_TOKEN=your_token' > .env"
