#!/usr/bin/env bash
# pidory-qa 환경 설치
# - pidory-qa worktree 만들기 (없으면)
# - pidory-qa.service 설치
# - /etc/pidory-qa/ + /var/lib/pidory-qa/ 디렉토리
# - config.qa.toml 생성 (없으면)
#
# 한 번만 실행하면 됨. 이후엔 qa-deploy.sh로 deploy.
set -euo pipefail

PROJECT_DIR=/home/deokdory/claude/projects/deokdory/pidory-qa
USER_NAME="$(whoami)"
HOME_DIR="$HOME"

# install-qa.sh가 위치한 source worktree 자동 감지 (PR γ branch 등)
SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_BRANCH="$(git -C "$SOURCE_DIR" branch --show-current 2>/dev/null || echo "")"

if [ -z "$SOURCE_BRANCH" ]; then
    echo "ERROR: source worktree branch detection 실패. install-qa.sh를 git worktree 안에서 실행해줘." >&2
    exit 1
fi

# 1. qa worktree 생성 (source branch 기준 — develop 의존 X)
if [ ! -d "$PROJECT_DIR" ]; then
    echo "[1/5] Creating qa worktree at $PROJECT_DIR (base: $SOURCE_BRANCH)..."
    git -C "$SOURCE_DIR" worktree add --detach "$PROJECT_DIR" "$SOURCE_BRANCH"
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

# 5. pidory-qa.service 설치 (source worktree의 unit 파일 사용)
echo "[5/5] Installing pidory-qa.service..."
sed -e "s|__USER__|$USER_NAME|g" \
    -e "s|__PROJECT_DIR__|$PROJECT_DIR|g" \
    -e "s|__HOME_DIR__|$HOME_DIR|g" \
    "$SOURCE_DIR/deploy/pidory-qa.service" | sudo tee /etc/systemd/system/pidory-qa.service > /dev/null
sudo systemctl daemon-reload

echo ""
echo "=== Done ==="
echo "qa worktree    : $PROJECT_DIR (base: $SOURCE_BRANCH)"
echo "infra source   : $SOURCE_DIR (모든 qa 명령은 여기서 호출)"
echo ""
echo "Next steps:"
echo "  1. .env.qa 파일에 token inject (deok-guard 키 둘 중 살아있는 것):"
echo "     deok-guard inject $PROJECT_DIR/.env.qa --env-line PIDORY_DEV_DISCORD_TOKEN"
echo "     # 또는: deok-guard inject $PROJECT_DIR/.env.qa --env-line pidory-dev/discord-token"
echo "  2. config.qa.toml 검토 — guild_id 본인 qa server인지"
echo "  3. postgres + qa db 셋업 (봇 자동 시작):"
echo "     sudo bash $SOURCE_DIR/scripts/postgres-qa-setup.sh"
echo "  4. (선택) 임의 branch deploy:"
echo "     bash $SOURCE_DIR/scripts/qa-deploy.sh <branch>"
