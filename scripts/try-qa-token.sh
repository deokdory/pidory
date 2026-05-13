#!/usr/bin/env bash
#
# try-qa-token.sh — deok-guard 두 키 중 어느 게 살아있는 dev-bot 토큰인지 자동 탐색
#
# Usage: bash try-qa-token.sh
#
# 흐름:
#   1. deok-guard inject로 PIDORY_DEV_DISCORD_TOKEN 환경변수에 첫 키 값 주입 (값 노출 X)
#   2. systemctl restart pidory-qa.service
#   3. 5초 대기 + journalctl로 인증 결과 판정
#   4. 401/실패면 두 번째 키로 같은 흐름
#   5. 첫 성공 키에서 멈춤
#
# 사전 조건:
#   - install-qa.sh 실행 완료 (qa worktree + service)
#   - postgres-qa-setup.sh 실행 완료 (/etc/pidory-qa/db.env 존재)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
QA_PROJECT_DIR=/home/deokdory/claude/projects/deokdory/pidory-qa
QA_ENV="$QA_PROJECT_DIR/.env.qa"
SERVICE=pidory-qa.service

# 두 키 후보 (deok-guard secret list 결과)
KEYS=("PIDORY_DEV_DISCORD_TOKEN" "pidory-dev/discord-token")

# 사전 조건 검사
if ! command -v deok-guard &>/dev/null; then
    echo "ERROR: deok-guard not installed" >&2
    exit 1
fi

if [ ! -d "$QA_PROJECT_DIR" ]; then
    echo "ERROR: qa worktree not found at $QA_PROJECT_DIR" >&2
    echo "  먼저 실행: bash $SCRIPT_DIR/deploy/install-qa.sh" >&2
    exit 1
fi

if [ ! -f /etc/systemd/system/pidory-qa.service ]; then
    echo "ERROR: pidory-qa.service 미설치" >&2
    echo "  먼저 실행: bash $SCRIPT_DIR/deploy/install-qa.sh" >&2
    exit 1
fi

if [ ! -f /etc/pidory-qa/db.env ]; then
    echo "ERROR: /etc/pidory-qa/db.env 없음" >&2
    echo "  먼저 실행: sudo bash $SCRIPT_DIR/scripts/postgres-qa-setup.sh" >&2
    exit 1
fi

echo "=== try-qa-token: deok-guard 두 키 자동 탐색 ==="
echo "qa env file : $QA_ENV"
echo "service     : $SERVICE"
echo ""

for KEY in "${KEYS[@]}"; do
    echo "--- Try: deok-guard secret '$KEY' ---"

    # 키 존재 확인
    if ! deok-guard secret exists "$KEY" &>/dev/null; then
        echo "  ⚠️  키 '$KEY' 가 deok-guard에 등록되어 있지 않음. 다음 키 시도..."
        echo ""
        continue
    fi

    # .env.qa 비우기 (이전 시도 잔재 제거)
    : > "$QA_ENV"
    chmod 600 "$QA_ENV"

    # deok-guard inject — 값 노출 X. 환경변수 이름은 PIDORY_DEV_DISCORD_TOKEN 고정 (config.qa.toml의 token_env)
    if ! deok-guard inject "$QA_ENV" --env-line "PIDORY_DEV_DISCORD_TOKEN=secret:${KEY}"; then
        echo "  ❌ inject 실패. 다음 키 시도..."
        echo ""
        continue
    fi

    # service restart
    echo "  service restart..."
    sudo systemctl restart "$SERVICE"

    # 5초 대기 후 1차 판정
    sleep 5
    LOG=$(sudo journalctl -u "$SERVICE" --since "10 seconds ago" --no-pager 2>&1)

    if echo "$LOG" | grep -qE "401|[Uu]nauthorized|[Ii]nvalid[[:space:]]*[Tt]oken"; then
        echo "  ❌ 401/Invalid Token. 다음 키 시도..."
        echo ""
        continue
    fi

    if echo "$LOG" | grep -qE "Database initialized|Logged in|ready event|gateway.*[Cc]onnected|Starting pidory v"; then
        echo ""
        echo "✅ 성공! 살아있는 키: $KEY"
        echo "   봇이 qa server에서 동작 중"
        echo ""
        echo "확인:"
        echo "   sudo systemctl status $SERVICE"
        echo "   sudo journalctl -u $SERVICE -f"
        echo ""
        echo "다음 단계 (PR α 격리 QA):"
        echo "   sudo cp /var/lib/pidory/pidory.db /var/lib/pidory-qa/pidory.db  # prod sqlite 복사"
        echo "   bash $SCRIPT_DIR/scripts/qa-deploy.sh 304-postgres-migration"
        exit 0
    fi

    # 5초 더 대기 (지연 시작 가능성)
    echo "  결과 불확실. 5초 더 대기..."
    sleep 5
    LOG=$(sudo journalctl -u "$SERVICE" --since "15 seconds ago" --no-pager 2>&1)

    if echo "$LOG" | grep -qE "Database initialized|Logged in|gateway.*[Cc]onnected"; then
        echo ""
        echo "✅ 성공 (지연 인증). 살아있는 키: $KEY"
        exit 0
    fi

    if echo "$LOG" | grep -qE "401|[Uu]nauthorized|[Ii]nvalid[[:space:]]*[Tt]oken"; then
        echo "  ❌ 지연 후에도 401. 다음 키 시도..."
    else
        echo "  ⚠️  여전히 불확실. 마지막 30줄 출력:"
        sudo journalctl -u "$SERVICE" -n 30 --no-pager | sed 's/^/    /'
        echo "  다음 키 시도..."
    fi
    echo ""
done

echo ""
echo "❌ 두 키 모두 인증 실패."
echo ""
echo "확인 항목:"
echo "  1. Discord Developer Portal에서 dev-bot application 살아있는지 (revoked 아닌지)"
echo "  2. dev-bot이 qa server (1504040703326556230)에 invite됐는지"
echo "  3. 정확한 에러 로그:"
echo "     sudo journalctl -u $SERVICE -n 50 --no-pager"
echo ""
echo "토큰 둘 다 죽었으면 Discord Developer Portal에서 새 발급 후 deok-guard에 재등록:"
echo "  deok-guard secret set PIDORY_DEV_DISCORD_TOKEN  # stdin으로 새 token 입력"
exit 1
