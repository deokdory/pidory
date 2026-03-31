#!/usr/bin/env bash
# statusline-ratelimit-writer.sh
#
# Extracts rate_limits from Claude Code statusLine stdin JSON and atomically
# writes the result to a file. Produces no stdout output so it does not
# pollute the calling statusLine script's output.
#
# Usage: Add to your Claude Code statusLine script:
#   input=$(cat)
#   echo "$input" | bash /path/to/statusline-ratelimit-writer.sh
#   # ... rest of your statusLine script using $input
#
# Output file path (default: /tmp/pidory-ratelimits.json):
#   Override with PIDORY_RATELIMIT_FILE env var.
#
# Output JSON format:
#   {"five_hour_pct":42,"seven_day_pct":38,"five_hour_reset":1774969200,"seven_day_reset":1775437200,"updated_at":UNIX_TS}
#
# If rate_limits is absent (e.g. Sonnet sessions), the file is not updated.

OUTPUT_FILE="${PIDORY_RATELIMIT_FILE:-/tmp/pidory-ratelimits.json}"

input=$(cat)

# Bail silently if rate_limits field is absent
if ! echo "$input" | jq -e '.rate_limits' >/dev/null 2>&1; then
    exit 0
fi

five_hour_pct=$(echo "$input" | jq -r '.rate_limits.five_hour.used_percentage // empty')
seven_day_pct=$(echo "$input" | jq -r '.rate_limits.seven_day.used_percentage // empty')
five_hour_reset=$(echo "$input" | jq -r '.rate_limits.five_hour.resets_at // empty')
seven_day_reset=$(echo "$input" | jq -r '.rate_limits.seven_day.resets_at // empty')
updated_at=$(date +%s)

payload=$(jq -n \
    --argjson five_hour_pct "${five_hour_pct:-0}" \
    --argjson seven_day_pct "${seven_day_pct:-0}" \
    --argjson five_hour_reset "${five_hour_reset:-0}" \
    --argjson seven_day_reset "${seven_day_reset:-0}" \
    --argjson updated_at "$updated_at" \
    '{five_hour_pct: $five_hour_pct, seven_day_pct: $seven_day_pct, five_hour_reset: $five_hour_reset, seven_day_reset: $seven_day_reset, updated_at: $updated_at}')

tmp=$(mktemp "${OUTPUT_FILE}.XXXXXX")
printf '%s\n' "$payload" > "$tmp"
mv "$tmp" "$OUTPUT_FILE"
