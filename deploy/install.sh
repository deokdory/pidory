#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== pidory deployment ==="

# Build
echo "[1/4] Building release binary..."
cd "$PROJECT_DIR"
cargo build --release

# Create .env with token (using deok-guard)
echo "[2/4] Setting up environment..."
if ! command -v deok-guard &>/dev/null; then
    echo "WARNING: deok-guard not found. Set PIDORY_DISCORD_TOKEN manually in .env"
else
    deok-guard env inject PIDORY_DISCORD_TOKEN --from secret:pidory/discord-token > "$PROJECT_DIR/.env"
fi

# Copy config if not exists
if [ ! -f "$PROJECT_DIR/config.toml" ]; then
    echo "[3/4] Creating config.toml from example..."
    cp "$PROJECT_DIR/config.toml.example" "$PROJECT_DIR/config.toml"
    echo "IMPORTANT: Edit config.toml with your Discord guild_id and owner_id"
else
    echo "[3/4] config.toml already exists, skipping"
fi

# Install systemd service
echo "[4/4] Installing systemd service..."
sudo cp "$SCRIPT_DIR/pidory.service" /etc/systemd/system/pidory.service
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
echo "  2. Set Discord token: deok-guard secret set pidory/discord-token YOUR_TOKEN"
