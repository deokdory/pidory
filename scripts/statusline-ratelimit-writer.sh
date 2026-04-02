#!/usr/bin/env bash
# statusline-ratelimit-writer.sh
#
# Extracts rate_limits and context_window from Claude Code statusLine stdin
# JSON and atomically writes the result to a file. Produces no stdout output
# so it does not pollute the calling statusLine script's output.
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
#   {"five_hour_pct":42,"seven_day_pct":38,"five_hour_reset":1774969200,"seven_day_reset":1775437200,"context_percent":45,"updated_at":UNIX_TS}
#
# If rate_limits is absent (e.g. Sonnet sessions), the file is not updated.
# context_percent is null when context_window data is unavailable.

OUTPUT_FILE="${PIDORY_RATELIMIT_FILE:-/tmp/pidory-ratelimits.json}"

input=$(cat)

# Only write when rate_limits is present to avoid clobbering valid data with zeros.
# context_percent is passed via --argjson since jq pipe loses parent context.
ctx_pct=$(echo "$input" | jq '.context_window.used_percentage // null' 2>/dev/null)
payload=$(echo "$input" | jq -c --argjson now "$(date +%s)" --argjson ctx "${ctx_pct:-null}" '
  .rate_limits // empty |
  if . == null then empty else
    {
      five_hour_pct: ((.five_hour.used_percentage // 0) | round),
      seven_day_pct: ((.seven_day.used_percentage // 0) | round),
      five_hour_reset: (.five_hour.resets_at // 0),
      seven_day_reset: (.seven_day.resets_at // 0),
      context_percent: (if $ctx then ($ctx | round) else null end),
      updated_at: $now
    }
  end
' 2>/dev/null) || exit 0

[ -z "$payload" ] && exit 0

tmp=$(mktemp "${OUTPUT_FILE}.XXXXXX")
printf '%s\n' "$payload" > "$tmp"
mv "$tmp" "$OUTPUT_FILE"
