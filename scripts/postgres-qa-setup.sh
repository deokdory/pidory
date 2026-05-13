#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# pidory postgres-qa-setup.sh
# Usage: sudo bash scripts/postgres-qa-setup.sh
# Sets up PostgreSQL for pidory-qa (isolated from prod).
# Creates role pidory_qa, database pidory_qa, writes /etc/pidory-qa/db.env,
# then restarts pidory-qa.service.
#
# PostgreSQL installation itself is idempotent — if prod already installed it,
# this script skips installation and only creates the qa role/db.
# =============================================================================

# ---------------------------------------------------------------------------
# 1. Root 권한 검사
# ---------------------------------------------------------------------------
if [ "$EUID" -ne 0 ]; then
    echo "ERROR: must be run as root (sudo bash scripts/postgres-qa-setup.sh)" >&2
    exit 1
fi

echo "=== pidory-qa PostgreSQL Setup ==="

# ---------------------------------------------------------------------------
# 2. 사전 조건 검사
# ---------------------------------------------------------------------------
if ! command -v openssl &>/dev/null; then
    echo "ERROR: openssl is required but not installed." >&2
    echo "  Install: apt-get install -y openssl" >&2
    exit 1
fi

if ! command -v systemctl &>/dev/null; then
    echo "ERROR: systemd is required but not found." >&2
    echo "  This script only supports Linux systemd environments." >&2
    exit 1
fi

if [ ! -f /etc/systemd/system/pidory-qa.service ]; then
    echo "ERROR: /etc/systemd/system/pidory-qa.service not found." >&2
    echo "  Please run deploy/install-qa.sh first." >&2
    exit 1
fi

# qa worktree 자체 binary 사용 (prod와 분리). qa-deploy.sh로 migrate-enabled branch deploy 시 빌드됨.
QA_MIGRATE_BIN=/home/deokdory/claude/projects/deokdory/pidory-qa/target/release/pidory-migrate
if [ ! -f "$QA_MIGRATE_BIN" ]; then
    echo "ERROR: $QA_MIGRATE_BIN not found." >&2
    echo "  Please run qa-deploy.sh with a migrate-enabled branch first:" >&2
    echo "    bash scripts/qa-deploy.sh 304-postgres-migration" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 3. deploy 사용자 자동 감지
# ---------------------------------------------------------------------------
# sudo 컨텍스트에서 stat -c '%U' /etc/pidory-qa 가 root (디렉토리가 root:user 0750)이라
# SUDO_USER를 우선. stat은 group(%G)으로 fallback (수동 chown 시나리오 대비).
DEPLOY_USER="${PIDORY_DEPLOY_USER:-${SUDO_USER:-$(stat -c '%G' /etc/pidory-qa 2>/dev/null || whoami)}}"
if [ "$DEPLOY_USER" = "root" ]; then
    echo "ERROR: DEPLOY_USER가 root로 감지됨. PIDORY_DEPLOY_USER env로 명시:" >&2
    echo "  sudo PIDORY_DEPLOY_USER=deokdory bash scripts/postgres-qa-setup.sh" >&2
    exit 1
fi
echo "[step 1/8] Deploy user: $DEPLOY_USER"

# ---------------------------------------------------------------------------
# 4. password 결정
# ---------------------------------------------------------------------------
if [ -n "${PIDORY_QA_DB_PASSWORD:-}" ]; then
    DB_PASSWORD="$PIDORY_QA_DB_PASSWORD"
    echo "[step 2/8] Using PIDORY_QA_DB_PASSWORD from environment"
else
    DB_PASSWORD="$(openssl rand -base64 24 | tr -d '/+= ')"
    echo "[step 2/8] Generated random database password"
fi

# ---------------------------------------------------------------------------
# 5. postgresql 설치 (idempotent — prod에서 이미 설치됐으면 skip)
# ---------------------------------------------------------------------------
echo "[step 3/8] Checking PostgreSQL installation..."
if dpkg -s postgresql-17 &>/dev/null; then
    echo "  postgresql-17 already installed, skipping"
else
    echo "  Installing postgresql-17..."
    if ! apt-get update -qq; then
        echo "WARNING: apt-get update failed, continuing anyway" >&2
    fi
    if ! apt-get install -y postgresql-17 2>/dev/null; then
        echo "WARNING: postgresql-17 not available, trying postgresql fallback..." >&2
        if ! apt-get install -y postgresql; then
            echo "ERROR: Failed to install PostgreSQL." >&2
            echo "  Check: apt-cache search postgresql" >&2
            exit 1
        fi
        echo "  Installed postgresql (fallback — not version 17)"
    else
        echo "  Installed postgresql-17"
    fi
fi

# ---------------------------------------------------------------------------
# 6. systemctl enable --now postgresql (idempotent)
# ---------------------------------------------------------------------------
echo "[step 4/8] Enabling and starting PostgreSQL..."
PG_SERVICE=""
for svc in postgresql postgresql@17-main postgresql@15-main postgresql@14-main; do
    if systemctl list-units --type=service --all | grep -q "${svc}.service"; then
        PG_SERVICE="$svc"
        break
    fi
done

if [ -z "$PG_SERVICE" ]; then
    PG_SERVICE="postgresql"
fi

if ! systemctl enable --now "$PG_SERVICE" 2>/dev/null; then
    systemctl enable "$PG_SERVICE" || true
    systemctl start "$PG_SERVICE" || true
fi

# Wait for postgres to be ready
for i in 1 2 3 4 5; do
    if sudo -u postgres psql -c '\q' &>/dev/null; then
        break
    fi
    echo "  Waiting for PostgreSQL to be ready... ($i/5)"
    sleep 2
done

if ! sudo -u postgres psql -c '\q' &>/dev/null; then
    echo "ERROR: PostgreSQL is not responding after startup." >&2
    echo "  Check: systemctl status $PG_SERVICE" >&2
    exit 1
fi

echo "  PostgreSQL is running"

# ---------------------------------------------------------------------------
# 7. role + database 생성 (idempotent) — qa 전용
# ---------------------------------------------------------------------------
echo "[step 5/8] Creating pidory_qa role and database..."

# password를 psql -v 변수로 전달 (literal quoting `:'password'` — 수동 escape 불필요, regress 안전)
if ! sudo -u postgres psql -v ON_ERROR_STOP=1 -v "qa_password=${DB_PASSWORD}" <<'EOF'
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pidory_qa') THEN
        EXECUTE format('CREATE ROLE pidory_qa LOGIN PASSWORD %L', :'qa_password');
    ELSE
        EXECUTE format('ALTER ROLE pidory_qa WITH LOGIN PASSWORD %L', :'qa_password');
    END IF;
END $$;
EOF
then
    echo "ERROR: [step 5/8] Failed to create/update pidory_qa role." >&2
    echo "  Check: sudo -u postgres psql -c '\\du'" >&2
    exit 1
fi

# Database 생성 (idempotent: 없을 때만)
if ! sudo -u postgres psql -v ON_ERROR_STOP=1 -tAc \
    "SELECT 1 FROM pg_database WHERE datname = 'pidory_qa'" | grep -q 1; then
    if ! sudo -u postgres createdb -O pidory_qa pidory_qa; then
        echo "ERROR: [step 5/8] Failed to create pidory_qa database." >&2
        echo "  Check: sudo -u postgres psql -l" >&2
        exit 1
    fi
    echo "  Created database 'pidory_qa'"
else
    echo "  Database 'pidory_qa' already exists, skipping"
fi

# ---------------------------------------------------------------------------
# 8. /etc/pidory-qa/ 디렉토리 (idempotent)
# ---------------------------------------------------------------------------
echo "[step 6/8] Ensuring /etc/pidory-qa/ directory..."
mkdir -p /etc/pidory-qa
chown "root:${DEPLOY_USER}" /etc/pidory-qa
chmod 0750 /etc/pidory-qa

# ---------------------------------------------------------------------------
# 9. /etc/pidory-qa/db.env 작성
# ---------------------------------------------------------------------------
echo "[step 7/8] Writing /etc/pidory-qa/db.env..."

# URL-encode password (special chars 처리)
URL_ESCAPED_PASSWORD=""
if command -v python3 &>/dev/null; then
    URL_ESCAPED_PASSWORD="$(python3 -c \
        'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' \
        "$DB_PASSWORD" 2>/dev/null)" || URL_ESCAPED_PASSWORD=""
fi

if [ -z "$URL_ESCAPED_PASSWORD" ] && command -v jq &>/dev/null; then
    URL_ESCAPED_PASSWORD="$(printf '%s' "$DB_PASSWORD" | jq -sRr @uri 2>/dev/null)" \
        || URL_ESCAPED_PASSWORD=""
fi

if [ -z "$URL_ESCAPED_PASSWORD" ]; then
    # Fallback: autoassigned password는 URL-safe 문자만 포함 (tr -d '/+= ' 결과)
    URL_ESCAPED_PASSWORD="$DB_PASSWORD"
fi

# db.env 진짜 atomic write — temp file을 /etc/pidory-qa 안에 생성 (root:user 0750 보호) 후 mv
# /tmp 같은 곳에 두면 secret 포함 temp file 누출 위험 + script 중단 시 잔존.
DB_ENV_TMP="$(mktemp /etc/pidory-qa/.db.env.XXXXXX)"
trap 'rm -f "$DB_ENV_TMP"' EXIT
chmod 0640 "$DB_ENV_TMP"
chown "root:${DEPLOY_USER}" "$DB_ENV_TMP"
printf 'DATABASE_URL=postgres://pidory_qa:%s@localhost/pidory_qa\n' "$URL_ESCAPED_PASSWORD" \
    > "$DB_ENV_TMP"
mv -f "$DB_ENV_TMP" /etc/pidory-qa/db.env
trap - EXIT  # mv 성공 후 cleanup 불필요

echo "  Written: /etc/pidory-qa/db.env (mode 0640, root:${DEPLOY_USER})"

# ---------------------------------------------------------------------------
# 10. systemctl daemon-reload + restart pidory-qa
# ---------------------------------------------------------------------------
echo "[step 8/8] Restarting pidory-qa.service..."
systemctl daemon-reload

if ! systemctl restart pidory-qa.service; then
    echo "ERROR: [step 8/8] pidory-qa.service failed to restart." >&2
    echo "  Diagnose: sudo journalctl -u pidory-qa.service -n 50 --no-pager" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 11. 3초 대기 후 is-active 확인
# ---------------------------------------------------------------------------
sleep 3

if systemctl is-active --quiet pidory-qa.service; then
    echo ""
    echo "pidory-qa 정상 동작 중"
    echo "   DATABASE_URL: postgres://pidory_qa:**@localhost/pidory_qa"
    echo "   password 확인: sudo cat /etc/pidory-qa/db.env"
    echo "   로그 확인:     sudo journalctl -u pidory-qa.service -f"
else
    echo ""
    echo "pidory-qa 시작 실패. 아래 명령으로 로그를 확인해줘:"
    echo "    sudo journalctl -u pidory-qa.service -n 50 --no-pager"
    exit 1
fi
