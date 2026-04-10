#!/usr/bin/env bash
#
# pidory prod deploy
#
# 1. pidory-prod worktree 에서 origin/master 로 reset
# 2. cargo build --release
# 3. pidory-delayed-restart.service 를 start (30초 후 pidory.service restart)
#
# 이 스크립트는 pidory.service 샌드박스 안에서 실행될 수 있도록 작성됨.
# - sudo 를 쓰지 않는다 (NoNewPrivileges=true)
# - systemctl 은 DBus + polkit 으로 동작 (50-pidory.rules)
# - 작업 디렉터리는 ReadWritePaths=local path/claude 범위 안
#
set -euo pipefail

DEPLOY_DIR=local path/claude/projects/deokdory/pidory-prod
DELAYED_UNIT=pidory-delayed-restart.service

if [ ! -d "$DEPLOY_DIR" ]; then
    echo "ERROR: deploy worktree not found at $DEPLOY_DIR" >&2
    echo "Create it with: git worktree add --detach $DEPLOY_DIR origin/master" >&2
    exit 1
fi

cd "$DEPLOY_DIR"

echo "[1/4] Fetching origin master..."
git fetch origin master

echo "[2/4] Resetting worktree to origin/master..."
git reset --hard origin/master
NEW_COMMIT=$(git rev-parse --short HEAD)
NEW_SUBJECT=$(git log -1 --format='%s')
echo "      -> $NEW_COMMIT $NEW_SUBJECT"

echo "[3/4] Building release binary..."
if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi
cargo build --release

echo "[4/4] Scheduling delayed restart..."
# 이전에 남아있을 수 있는 실패/대기 상태 정리
systemctl reset-failed "$DELAYED_UNIT" 2>/dev/null || true
systemctl stop "$DELAYED_UNIT" 2>/dev/null || true
# 30초 후 pidory.service restart (유닛 파일에 ExecStartPre=sleep 30 내장)
systemctl start --no-block "$DELAYED_UNIT"

cat <<EOF

=== prod deploy scheduled ===
commit  : $NEW_COMMIT
subject : $NEW_SUBJECT
restart : 약 30초 후 pidory.service 재시작됨
EOF
