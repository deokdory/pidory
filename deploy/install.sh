#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
USER_NAME="$(whoami)"
HOME_DIR="$HOME"

echo "=== pidory deployment ==="

# 1. Build
echo "[1/4] Building release binary..."
cd "$PROJECT_DIR"
cargo build --release

# 2. Check .env
echo "[2/4] Checking environment..."
if [ ! -f "$PROJECT_DIR/.env" ]; then
    echo "WARNING: .env not found."
    echo "Create it with: echo 'PIDORY_DISCORD_TOKEN=your_token' > $PROJECT_DIR/.env"
fi

# 3. Copy config if not exists
if [ ! -f "$PROJECT_DIR/config.toml" ]; then
    echo "[3/4] Creating config.toml from example..."
    cp "$PROJECT_DIR/config.toml.example" "$PROJECT_DIR/config.toml"
    echo "IMPORTANT: Edit config.toml with your Discord guild_id and owner_id"
else
    echo "[3/4] config.toml already exists, skipping"
fi

# 4. Install systemd service (sed 치환)
echo "[4/4] Installing systemd service..."
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
echo ""
echo "Don't forget to:"
echo "  1. Edit config.toml with your Discord guild_id and owner_id"
echo "  2. Set Discord token in .env: echo 'PIDORY_DISCORD_TOKEN=your_token' > .env"
