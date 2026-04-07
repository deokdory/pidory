# pidory

<p align="center">
  <img src="assets/pidory.png" width="256" alt="pidory">
</p>

**English** | [한국어](README.ko.md)

Discord ↔ Claude Code CLI bridge. Send messages in a Discord thread and get Claude Code responses — tool permission prompts appear as interactive buttons, and long outputs are split or attached automatically.

## Features

- Thread-based conversations mapped to Claude Code sessions
- Long-lived process with message queue + mid-turn message injection
- Tool permission approve/deny with Discord buttons (Allow / Always Allow / Deny)
- Real-time intermediate status display
- Code block-aware message splitting for Discord's 2000 char limit
- Rate limit monitoring — bot presence shows current usage %, with configurable threshold alerts
- Session lifecycle management — LRU auto-eviction when max sessions reached, idle timeout cleanup
- Streaming messages sent without push notifications to reduce spam

## Security Model

pidory delegates to Discord's built-in permission system, and sessions are **shared per thread**.

- Anyone who can access the channel where a thread is registered can use the bot (the channel's VIEW_CHANNEL / SEND_MESSAGES permissions act as the gate).
- Users in the same thread **share the same Claude Code session**. This means:
  - A tool permission granted as `Always Allow` by one user applies **to the entire session** — subsequent messages from other users in the same thread will be auto-approved for that tool.
  - `/skill` can be invoked by any member and can run arbitrary Claude Code skills in the session.
- Administrative commands (`/register`, `/unregister`, `/del`, `/status`, `/list`, `/sessions`) require `MANAGE_GUILD` or `MANAGE_CHANNELS` permissions respectively.
- `/stop` can only be called by the user who started the current turn (or the `owner_id`).

**⚠️ Multi-user support is still in beta.** Use with caution.

This model assumes that users invited to the guild **trust each other**. Only invite people you **genuinely trust** to the server running pidory and work together. Otherwise a malicious user could use the bot to execute arbitrary code, manipulate files, or escalate permissions on behalf of other users.

## Prerequisites

- Rust 1.85+ (2024 edition)
- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) — requires Anthropic Max subscription
- Discord Bot Token
- Linux or macOS

## Quick Start

### 1. Create Discord Bot

1. Go to https://discord.com/developers/applications → **New Application**
2. Name it, then go to the **Bot** tab
3. Click **Reset Token** → copy the token (shown once only)
4. Under **Privileged Gateway Intents**, enable **MESSAGE CONTENT INTENT**
5. Go to **OAuth2** → **URL Generator**:
   - Scopes: `bot`, `applications.commands`
   - Bot Permissions: `Send Messages`, `Read Message History`, `Add Reactions`, `Manage Messages`, `Use Slash Commands`, `Embed Links`, `Attach Files`
6. Open the generated URL and invite the bot to your server

### 2. Get Discord IDs

1. In Discord: **Settings** → **Advanced** → enable **Developer Mode**
2. Right-click your server icon → **Copy Server ID** → this is `guild_id`
3. Right-click your own profile → **Copy User ID** → this is `owner_id`

### 3. Clone & Configure

```bash
git clone https://github.com/deokdory/pidory.git
cd pidory
cp config.toml.example config.toml
# Edit config.toml — set guild_id and owner_id
```

### 4. Set Discord Token

```bash
echo 'PIDORY_DISCORD_TOKEN=your_token_here' > .env
```

### 5. Run

```bash
# Direct
cargo run --release

# Or as a service
./deploy/install.sh   # auto-detects Linux (systemd) or macOS (launchd)
```

## Service Deployment

### Linux (systemd)

```bash
./deploy/install.sh
sudo systemctl start pidory
sudo systemctl status pidory
journalctl -u pidory -f
```

### macOS (launchd)

```bash
./deploy/install.sh
launchctl load ~/Library/LaunchAgents/com.pidory.bot.plist
tail -f ~/.pidory/stderr.log
```

`install.sh` builds the release binary, copies `config.toml.example` if no config exists, installs the service file, enables it on boot, and deploys built-in skills to `~/.claude/skills/`.

## Update

To update an existing installation:

```bash
cd pidory
./deploy/update.sh
```

This will:
1. Pull latest changes (fast-forward only)
2. Rebuild the release binary
3. Sync built-in skills to `~/.claude/skills/`

After update, restart the service:
- **Linux**: `sudo systemctl restart pidory`
- **macOS**: `launchctl kickstart -k gui/$(id -u)/com.pidory.bot`

### Menubar app (macOS only)

A small menu bar app shows pidory's current state at a glance and lets you pull / build / restart with one click.

```bash
./tools/menubar/install.sh
```

The icon reflects the current state — `✓` synced, `⬇` pull needed, `🔨` build needed, `↻` restart needed, `⏳` working, `⚠` last action failed. Click for status detail and actions, including **Update everything** which chains pull → build → restart automatically. Background polling is cheap (mtime-cached, 5-min interval). Uninstall with `./tools/menubar/uninstall.sh`.

## Usage

### Slash Commands

All commands are owner-only (restricted to the `owner_id` set in `config.toml`).

| Command | Description |
|---------|-------------|
| `/register <path> [name]` | Register a project directory to the current channel |
| `/unregister` | Unregister the project from the current channel |
| `/list [channel]` | List active sessions for a channel |
| `/del [thread_id]` | Delete a session (defaults to current thread) |
| `/stop` | Stop the current session's Claude Code process |
| `/status [thread_id]` | Show session status (defaults to current thread) |
| `/skill <name>` | Send a slash command (e.g. `/commit`) to the Claude Code session |
| /sessions | Show global session overview (count, idle time, status) |

### Chatting with Claude Code

1. Run `/register /path/to/your/project` in any channel
2. Start a thread in that channel — each thread is its own Claude Code session
3. Send messages in the thread to chat with Claude Code
4. When Claude Code requests tool permissions, respond with the Discord buttons:
   - **Allow** — allow this once
   - **Always Allow** — add to the always-allowed list for this session
   - **Deny** — deny the tool call

## Configuration

`config.toml` fields (see `config.toml.example`):

| Field | Description | Default |
|-------|-------------|---------|
| `discord.guild_id` | Your Discord server ID | — |
| `discord.owner_id` | Your Discord user ID (bot owner) | — |
| `claude.binary_path` | Path to the `claude` CLI binary | `"claude"` |
| `claude.default_disallowed_tools` | Tools to block by default | `[]` |
| `claude.subprocess_timeout_secs` | Max time per Claude Code subprocess | `600` |
| `claude.max_sessions` | Max concurrent sessions | `10` |
| `claude.idle_timeout_secs` | Idle session timeout in seconds (0 to disable) | `7200` |
| `discord.notification_channel_id` | Channel ID for rate limit alerts (optional) | — |
| `response.max_chunk_length` | Max characters per Discord message | `1900` |
| `response.max_chunks` | Chunks before falling back to file attachment | `10` |
| `ratelimit.file_path` | Path to the rate limit JSON file (optional) | — |
| `ratelimit.update_interval_secs` | How often to read the rate limit file | `60` |
| `ratelimit.alert_thresholds` | 5h usage % thresholds that trigger alerts | `[50, 80]` |

The Discord token is read from the `PIDORY_DISCORD_TOKEN` environment variable (or `.env` file) — never put it in `config.toml`.

## Rate Limit Monitoring

pidory can display Claude Code's API rate limit usage as a Discord bot presence (e.g. `Watching 5h: 42%(1h30m) | 7d: 38%`) and send alerts when thresholds are exceeded.

### How it works

```
Claude Code statusLine hook → writes /tmp/pidory-ratelimits.json
pidory reads the file periodically → updates bot presence + sends alerts
```

Claude Code's `statusLine` receives rate limit data as JSON on stdin. A helper script extracts the usage percentages and writes them to a file that pidory monitors.

### Setup

1. Add the ratelimit writer to your Claude Code statusLine script (`~/.claude/settings.json`):

```bash
# In your statusLine script, after reading stdin:
input=$(cat)
echo "$input" | bash /path/to/pidory/scripts/statusline-ratelimit-writer.sh 2>/dev/null
# ... rest of your statusLine script
```

2. Enable monitoring in `config.toml`:

```toml
[ratelimit]
file_path = "/tmp/pidory-ratelimits.json"
# update_interval_secs = 60
# alert_thresholds = [50, 80]
```

3. Optionally set `notification_channel_id` under `[discord]` to receive threshold alerts in a specific channel.

## License

MIT
