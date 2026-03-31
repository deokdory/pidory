#!/usr/bin/env bash
# statusline-wrapper.sh — Claude Code statusLine transparent wrapper
#
# Reads statusLine JSON from stdin, writes rate limit data to file,
# then passes the JSON through to the user's original statusLine script.
#
# Setup (in ~/.claude/settings.json):
#   "statusLine": {
#     "type": "command",
#     "command": "bash /path/to/statusline-wrapper.sh --chain 'bash /path/to/your-statusline.sh'"
#   }
#
# Options:
#   --chain <command>   Original statusLine command to chain (required)
#
# Environment:
#   PIDORY_RATELIMIT_FILE  Output file path (default: /tmp/pidory-ratelimits.json)

CHAIN_CMD=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --chain)
            CHAIN_CMD="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

input=$(cat)

# Write ratelimit file (synchronous — fast enough, avoids background job noise)
echo "$input" | bash "$(dirname "$0")/statusline-ratelimit-writer.sh" 2>/dev/null

# Chain to original statusLine script (or just pass through)
if [ -n "$CHAIN_CMD" ]; then
    echo "$input" | eval "$CHAIN_CMD"
fi
