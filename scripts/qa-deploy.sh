#!/usr/bin/env bash
#
# pidory qa worktree deploy
#
# 임의 feature 브랜치를 qa-bot 환경에 올린다. prod와 완전 격리.
#
# 1. pidory-qa worktree에서 origin/<branch> reset
# 2. cargo build --release + cargo build --bin pidory-migrate --features migrate --release
# 3. pidory-qa.service restart (delayed 없이 즉시 — qa는 사용자 영향 없음)
#
# Usage: qa-deploy.sh <branch>
#
set -euo pipefail

if [ $# -lt 1 ] || [ -z "${1:-}" ]; then
    echo "ERROR: branch name required" >&2
    echo "Usage: $0 <branch>" >&2
    exit 2
fi

BRANCH="$1"
DEPLOY_DIR=local path/claude/projects/deokdory/pidory-qa

if [ ! -d "$DEPLOY_DIR" ]; then
    echo "ERROR: qa worktree not found at $DEPLOY_DIR" >&2
    echo "Create it with: git worktree add --detach $DEPLOY_DIR origin/develop" >&2
    exit 1
fi

cd "$DEPLOY_DIR"

echo "[1/4] Fetching origin $BRANCH..."
git fetch origin "$BRANCH"

echo "[2/4] Resetting worktree to origin/$BRANCH..."
git reset --hard "origin/$BRANCH"
NEW_COMMIT=$(git rev-parse --short HEAD)
NEW_SUBJECT=$(git log -1 --format='%s')
echo "      -> $NEW_COMMIT $NEW_SUBJECT"

echo "[3/4] Building release binaries..."
if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi
cargo build --release
# pidory-migrate도 빌드 (migrate feature 있는 branch만). qa worktree 자체 target/release/에 둠 —
# pidory-qa.service의 ExecStartPre가 직접 사용. prod의 /usr/local/bin/pidory-migrate 안 건드림 (isolation).
if grep -q 'name = "pidory-migrate"' Cargo.toml 2>/dev/null; then
    cargo build --bin pidory-migrate --features migrate --release
fi

echo "[4/4] Restarting pidory-qa.service..."
sudo systemctl restart pidory-qa.service

cat <<EOF

=== qa deploy complete ===
branch  : $BRANCH
commit  : $NEW_COMMIT
subject : $NEW_SUBJECT
service : pidory-qa.service (restarted immediately)
logs    : sudo journalctl -u pidory-qa.service -f
EOF
