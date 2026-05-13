#!/usr/bin/env bash
# pidory-qa 환경 설치
# - pidory-qa worktree 만들기 (없으면)
# - pidory-qa.service 설치
# - /etc/pidory-qa/ + /var/lib/pidory-qa/ 디렉토리
# - config.qa.toml 생성 (없으면)
#
# 한 번만 실행하면 됨. 이후엔 qa-deploy.sh로 deploy.
set -euo pipefail

PROJECT_DIR=local path/claude/projects/deokdory/pidory-qa
USER_NAME="$(whoami)"
HOME_DIR="$HOME"

# 1. qa worktree 생성 (main clone에서)
MAIN_CLONE=local path/claude/projects/deokdory/pidory
if [ ! -d "$PROJECT_DIR" ]; then
    echo "[1/5] Creating qa worktree..."
    cd "$MAIN_CLONE"
    git worktree add --detach "$PROJECT_DIR" origin/develop
else
    echo "[1/5] qa worktree already exists at $PROJECT_DIR"
fi

cd "$PROJECT_DIR"

# 2. config.qa.toml 생성
if [ ! -f "$PROJECT_DIR/config.qa.toml" ]; then
    echo "[2/5] Creating config.qa.toml from example..."
    cp "$PROJECT_DIR/config.qa.toml.example" "$PROJECT_DIR/config.qa.toml"
    echo "IMPORTANT: Edit config.qa.toml — guild_id가 본인 qa server인지 확인"
else
    echo "[2/5] config.qa.toml already exists, skipping"
fi

# 3. /var/lib/pidory-qa/ 디렉토리
echo "[3/5] Creating /var/lib/pidory-qa/..."
sudo mkdir -p /var/lib/pidory-qa
sudo chown "$USER_NAME:$USER_NAME" /var/lib/pidory-qa
sudo chmod 700 /var/lib/pidory-qa

# 4. /etc/pidory-qa/ 디렉토리 (postgres-qa-setup.sh가 db.env 작성)
echo "[4/5] Creating /etc/pidory-qa/..."
if [ ! -d /etc/pidory-qa ]; then
    sudo mkdir -p /etc/pidory-qa
    sudo chown "root:$USER_NAME" /etc/pidory-qa
    sudo chmod 0750 /etc/pidory-qa
else
    echo "  /etc/pidory-qa already exists, skipping"
fi

# 5. pidory-qa.service 설치
echo "[5/5] Installing pidory-qa.service..."
sed -e "s|__USER__|$USER_NAME|g" \
    -e "s|__PROJECT_DIR__|$PROJECT_DIR|g" \
    -e "s|__HOME_DIR__|$HOME_DIR|g" \
    "$MAIN_CLONE/deploy/pidory-qa.service" | sudo tee /etc/systemd/system/pidory-qa.service > /dev/null
sudo systemctl daemon-reload

echo ""
echo "=== Done ==="
echo "Next steps:"
echo "  1. .env.qa 파일에 PIDORY_DEV_DISCORD_TOKEN 주입:"
echo "     deok-guard inject $PROJECT_DIR/.env.qa --env-line PIDORY_DEV_DISCORD_TOKEN"
echo "     (또는 키 이름이 'pidory-dev/discord-token'이면 그것 사용)"
echo "  2. config.qa.toml 검토 — guild_id 본인 qa server인지"
echo "  3. PR α (#307 postgres) 코드를 qa로 deploy:"
echo "     bash scripts/qa-deploy.sh 304-postgres-migration"
echo "  4. postgres + qa db 셋업:"
echo "     sudo bash scripts/postgres-qa-setup.sh"
