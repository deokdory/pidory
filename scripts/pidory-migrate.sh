#!/bin/bash
set -euo pipefail

echo "=== pidory-migrate verification ==="

DB_ENV_FILE="/etc/pidory/db.env"
if [ -f "$DB_ENV_FILE" ]; then
    # shellcheck source=/dev/null
    set -a
    source "$DB_ENV_FILE"
    set +a
    echo "  Sourced $DB_ENV_FILE"
else
    echo "  ⚠️  $DB_ENV_FILE not found. Relying on existing DATABASE_URL env."
fi

if [ -z "${DATABASE_URL:-}" ]; then
    echo "  ❌ DATABASE_URL not set."
    echo "     Run: bash scripts/postgres-setup.sh"
    exit 2
fi

echo "  Running pidory-migrate..."
echo ""

set +e
pidory-migrate
RC=$?
set -e

echo ""
case "$RC" in
    0)
        echo "  ✅ migration succeeded (see log above for details)"
        ;;
    1)
        echo "  ❌ migration failed (runtime error). Check log above."
        ;;
    2)
        echo "  ❌ legacy SQLite database not found. See migrate error above."
        echo "     Set PIDORY_LEGACY_DB env or [database] path in config.toml."
        ;;
    *)
        echo "  ❓ migration exited with unexpected code $RC. Investigate."
        ;;
esac

# Interactive pause (TTY only)
if [ -t 0 ] && [ -t 1 ]; then
    echo ""
    read -p "Press Enter to continue..." _
fi

exit "$RC"
