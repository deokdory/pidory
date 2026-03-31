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

# Extract all fields in a single jq call (avoids 6 separate forks)
payload=$(echo "$input" | jq -c --argjson now "$(date +%s)" '
  .rate_limits // empty |
  if . == null then empty else
    {
      five_hour_pct: (.five_hour.used_percentage // 0),
      seven_day_pct: (.seven_day.used_percentage // 0),
      five_hour_reset: (.five_hour.resets_at // 0),
      seven_day_reset: (.seven_day.resets_at // 0),
      updated_at: $now
    }
  end
' 2>/dev/null) || exit 0

[ -z "$payload" ] && exit 0

tmp=$(mktemp "${OUTPUT_FILE}.XXXXXX")
printf '%s\n' "$payload" > "$tmp"
mv "$tmp" "$OUTPUT_FILE"
