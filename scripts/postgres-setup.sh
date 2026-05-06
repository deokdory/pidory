#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# pidory postgres-setup.sh
# Usage: sudo bash scripts/postgres-setup.sh
# Sets up PostgreSQL for pidory, writes /etc/pidory/db.env, restarts service.
# =============================================================================

# ---------------------------------------------------------------------------
# 1. Root 권한 검사
# ---------------------------------------------------------------------------
if [ "$EUID" -ne 0 ]; then
    echo "ERROR: must be run as root (sudo bash scripts/postgres-setup.sh)" >&2
    exit 1
fi

echo "=== pidory PostgreSQL Setup ==="

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

if [ ! -f /etc/systemd/system/pidory.service ]; then
    echo "ERROR: /etc/systemd/system/pidory.service not found." >&2
    echo "  Please run deploy/install.sh first." >&2
    exit 1
fi

if [ ! -f /usr/local/bin/pidory-migrate ]; then
    echo "ERROR: /usr/local/bin/pidory-migrate not found." >&2
    echo "  Please run deploy/install.sh first." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 3. deploy 사용자 자동 감지
# ---------------------------------------------------------------------------
DEPLOY_USER="${PIDORY_DEPLOY_USER:-$(stat -c '%U' /etc/pidory 2>/dev/null || echo "${SUDO_USER:-root}")}"
echo "[step 1/8] Deploy user: $DEPLOY_USER"

# ---------------------------------------------------------------------------
# 4. password 결정
# ---------------------------------------------------------------------------
if [ -n "${PIDORY_DB_PASSWORD:-}" ]; then
    DB_PASSWORD="$PIDORY_DB_PASSWORD"
    echo "[step 2/8] Using PIDORY_DB_PASSWORD from environment"
else
    DB_PASSWORD="$(openssl rand -base64 24 | tr -d '/+= ')"
    echo "[step 2/8] Generated random database password"
fi

# ---------------------------------------------------------------------------
# 5. postgresql 설치 (idempotent)
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
# Detect the actual postgresql service name (may be postgresql@17-main or postgresql)
PG_SERVICE=""
for svc in postgresql postgresql@17-main postgresql@15-main postgresql@14-main; do
    if systemctl list-units --type=service --all | grep -q "${svc}.service"; then
        PG_SERVICE="$svc"
        break
    fi
done

if [ -z "$PG_SERVICE" ]; then
    # fallback: just try postgresql
    PG_SERVICE="postgresql"
fi

if ! systemctl enable --now "$PG_SERVICE" 2>/dev/null; then
    # Some versions use versioned service name via meta unit; try without --now
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
# 7. role + database 생성 (idempotent)
# ---------------------------------------------------------------------------
echo "[step 5/8] Creating pidory role and database..."

# SQL injection 방지: single quote escape
ESCAPED_PASSWORD="${DB_PASSWORD//\'/\'\'}"

# Role 생성 또는 password 갱신
if ! sudo -u postgres psql -v ON_ERROR_STOP=1 <<EOF
DO \$\$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pidory') THEN
        CREATE ROLE pidory LOGIN PASSWORD '${ESCAPED_PASSWORD}';
    ELSE
        ALTER ROLE pidory WITH LOGIN PASSWORD '${ESCAPED_PASSWORD}';
    END IF;
END \$\$;
EOF
then
    echo "ERROR: [step 5/8] Failed to create/update pidory role." >&2
    echo "  Check: sudo -u postgres psql -c '\\du'" >&2
    exit 1
fi

# Database 생성 (idempotent: 없을 때만)
if ! sudo -u postgres psql -v ON_ERROR_STOP=1 -tAc \
    "SELECT 1 FROM pg_database WHERE datname = 'pidory'" | grep -q 1; then
    if ! sudo -u postgres createdb -O pidory pidory; then
        echo "ERROR: [step 5/8] Failed to create pidory database." >&2
        echo "  Check: sudo -u postgres psql -l" >&2
        exit 1
    fi
    echo "  Created database 'pidory'"
else
    echo "  Database 'pidory' already exists, skipping"
fi

# ---------------------------------------------------------------------------
# 8. /etc/pidory/ 디렉토리 (idempotent)
# ---------------------------------------------------------------------------
echo "[step 6/8] Ensuring /etc/pidory/ directory..."
mkdir -p /etc/pidory
chown "root:${DEPLOY_USER}" /etc/pidory
chmod 0750 /etc/pidory

# ---------------------------------------------------------------------------
# 9. /etc/pidory/db.env 작성
# ---------------------------------------------------------------------------
echo "[step 7/8] Writing /etc/pidory/db.env..."

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
    # 사용자 override의 경우 특수문자 위험 있으나 최선의 fallback
    URL_ESCAPED_PASSWORD="$DB_PASSWORD"
fi

# db.env 작성 (atomic write via install)
DB_ENV_TMP="$(mktemp)"
printf 'DATABASE_URL=postgres://pidory:%s@localhost/pidory\n' "$URL_ESCAPED_PASSWORD" \
    > "$DB_ENV_TMP"
install -m 0640 -o root -g "$DEPLOY_USER" "$DB_ENV_TMP" /etc/pidory/db.env
rm -f "$DB_ENV_TMP"

echo "  Written: /etc/pidory/db.env (mode 0640, root:${DEPLOY_USER})"

# ---------------------------------------------------------------------------
# 10. systemctl daemon-reload + restart pidory
# ---------------------------------------------------------------------------
echo "[step 8/8] Restarting pidory.service..."
systemctl daemon-reload

if ! systemctl restart pidory.service; then
    echo "ERROR: [step 8/8] pidory.service failed to restart." >&2
    echo "  Diagnose: sudo journalctl -u pidory.service -n 50 --no-pager" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 11. 3초 대기 후 is-active 확인
# ---------------------------------------------------------------------------
sleep 3

if systemctl is-active --quiet pidory.service; then
    echo ""
    echo "✅ pidory 정상 동작 중"
    echo "   DATABASE_URL: postgres://pidory:**@localhost/pidory"
    echo "   password 확인: sudo cat /etc/pidory/db.env"
    echo "   로그 확인:     sudo journalctl -u pidory.service -f"
else
    echo ""
    echo "❌ pidory 시작 실패. 아래 명령으로 로그를 확인해줘:"
    echo "    sudo journalctl -u pidory.service -n 50 --no-pager"
    exit 1
fi
