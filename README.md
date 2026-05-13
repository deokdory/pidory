# pidory

<p align="center">
  <img src="assets/pidory.png" width="256" alt="pidory">
</p>

**English** | [한국어](README.ko.md)

Discord ↔ Claude Code CLI bridge. Send messages in a Discord thread and get Claude Code responses — tool permission prompts appear as interactive buttons, and long outputs are split or attached automatically.

## Features

- **Thread-based conversations** — each thread maps to an independent Claude Code session
- **Long-lived process** — message queue with mid-turn message injection
- **Tool permissions** — approve/deny with Discord buttons (Allow / Always Allow / Deny)
- **Interactive questions** — Claude Code's `AskUserQuestion` rendered as buttons, select menus, or modal text input
- **File attachments** — upload files from Discord to Claude Code, and Claude Code can send files back to Discord
- **Reply context** — reply to a Discord message to include it as context in your prompt
- **Progress indicator** — real-time display of long-running tool executions
- **Message splitting** — code block-aware splitting for Discord's 2000 char limit, with automatic file attachment fallback
- **Rate limit monitoring** — bot presence shows current usage %, with configurable threshold alerts
- **Session lifecycle** — LRU auto-eviction when max sessions reached, idle timeout cleanup
- **Notification suppression** — streaming intermediate messages sent without push notifications
- **Multi-language UI** — Korean (default) and English

## Security Model

pidory delegates to Discord's built-in permission system, and sessions are **shared per thread**.

- Anyone who can access the channel where a thread is registered can use the bot (the channel's VIEW_CHANNEL / SEND_MESSAGES permissions act as the gate).
- Users in the same thread **share the same Claude Code session**. This means:
  - A tool permission granted as `Always Allow` by one user applies **to the entire session** — subsequent messages from other users in the same thread will be auto-approved for that tool.
  - `/skill` can be invoked by any member and can run arbitrary Claude Code skills in the session.
- Administrative commands (`/register`, `/unregister`, `/del`, `/status`, `/list`, `/sessions`) require `MANAGE_GUILD` or `MANAGE_CHANNELS` permissions.
- `/stop` can only be called by the user who started the current turn (or the `owner_id`).
- Permission buttons (Allow / Always Allow / Deny) can only be clicked by the user who started the current turn (or the `owner_id`).

**Warning: Multi-user support is still in beta.** This model assumes that users invited to the guild **trust each other**. Only invite people you **genuinely trust** to the server running pidory. Otherwise a malicious user could use the bot to execute arbitrary code, manipulate files, or escalate permissions on behalf of other users.

## Prerequisites

- Rust 1.85+ (2024 edition)
- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) — requires Anthropic Max subscription
- Discord Bot Token
- Linux or macOS
- PostgreSQL 17 (recommended) or system default — installed automatically by `scripts/postgres-setup.sh` on Linux

## Quick Start

### 1. Create Discord Bot

1. Go to https://discord.com/developers/applications → **New Application**
2. Name it, then go to the **Bot** tab
3. Click **Reset Token** → copy the token (shown once only)
4. Under **Privileged Gateway Intents**, enable **MESSAGE CONTENT INTENT**
5. Go to **OAuth2** → **URL Generator**:
   - Scopes: `bot`, `applications.commands`
   - Bot Permissions: `Send Messages`, `Read Message History`, `Add Reactions`, `Manage Messages`, `Manage Channels`, `Create Public Threads`, `Use Slash Commands`, `Embed Links`, `Attach Files`
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

**Service deployment (recommended — Linux systemd):**

```bash
./deploy/install.sh                 # builds binary, installs systemd service, installs skills
sudo bash scripts/postgres-setup.sh # installs PostgreSQL, creates DB, writes /etc/pidory/db.env, restarts service
```

That's it. The service starts automatically and connects to PostgreSQL via `DATABASE_URL` in `/etc/pidory/db.env`.

**Manual / dev (no service):**

```bash
# Set DATABASE_URL pointing to your PostgreSQL instance
export DATABASE_URL=postgres://pidory:<your-password>@localhost/pidory

cargo run --release
```

Verify the service is healthy:

```bash
sudo systemctl status pidory
sudo journalctl -u pidory.service -f
psql -U pidory -d pidory -c 'SELECT count(*) FROM projects'
```

## Service Deployment

### Linux (systemd)

```bash
./deploy/install.sh                  # builds binary, installs service, installs skills
sudo bash scripts/postgres-setup.sh # sets up PostgreSQL and starts the service
sudo systemctl status pidory
journalctl -u pidory.service -f
```

`install.sh` builds the release binary, copies `config.toml.example` if no config exists, installs the service file, enables it on boot, installs the `pidory-migrate` migration binary to `/usr/local/bin/`, and deploys built-in skills to `~/.claude/skills/`.

`postgres-setup.sh` installs PostgreSQL 17 (falls back to system default), creates the `pidory` role and database, writes `DATABASE_URL` to `/etc/pidory/db.env` (mode 600), and restarts the service.

### macOS (launchd)

```bash
./deploy/install.sh
launchctl load ~/Library/LaunchAgents/com.pidory.bot.plist
tail -f ~/.pidory/stderr.log
```

`install.sh` builds the release binary, copies `config.toml.example` if no config exists, installs the service file, enables it on boot, and deploys built-in skills to `~/.claude/skills/`. Note: `postgres-setup.sh` is Linux/systemd only — on macOS, set `DATABASE_URL` manually before running.

## Database (PostgreSQL)

pidory uses PostgreSQL as its database backend. The connection is configured via the `DATABASE_URL` environment variable — **this is the authoritative source**. The `[database] path` field in `config.toml` is deprecated and ignored at runtime.

### DATABASE_URL

On Linux service deployments, `DATABASE_URL` is injected from `/etc/pidory/db.env` (via systemd `EnvironmentFile`). The file is written by `scripts/postgres-setup.sh` and has mode 600 (readable only by the pidory service user).

```
DATABASE_URL=postgres://pidory:<password>@localhost/pidory
```

To inspect the value:

```bash
sudo cat /etc/pidory/db.env
```

For manual or dev setups, export the variable before running:

```bash
export DATABASE_URL=postgres://pidory:<your-password>@localhost/pidory
cargo run --release
```

You can override the legacy SQLite path with `PIDORY_LEGACY_DB` (default: `/var/lib/pidory/pidory.db`). This is only relevant for the migration binary (`pidory-migrate`), which reads the SQLite source on first-run and imports existing data into PostgreSQL.

### Automatic Migration

pidory uses a two-layer migration safety net:

1. **`pidory-migrate` ExecStartPre** — runs before the service starts. Detects if the PostgreSQL database is empty and performs a one-time, transactional import from the SQLite source. Idempotent: subsequent runs are no-ops.
2. **`sqlx::migrate!` in `init_pool`** — applies any pending schema migrations at startup. Runs every start, handles version upgrades automatically.

Migration is **roll-forward only**. There is no tool to revert from PostgreSQL back to SQLite.

### PostgreSQL Version

PostgreSQL 17 is recommended. `scripts/postgres-setup.sh` installs `postgresql-17` via apt; if unavailable, it falls back to the system default `postgresql` package. Other versions (14, 15, 16) are expected to work.

### Known Limitation

The self-update rollback path in `update/backup.rs` (`restore_db()`) uses the `sqlite3` CLI and does not support PostgreSQL. This means the automatic database restore during a failed self-update does not function in PostgreSQL environments. Normal operation is unaffected. Manual recovery via `pg_dump` / `psql` is the workaround. A follow-up PR will convert this to a `pg_dump`-based implementation.

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

## Usage

### Slash Commands

| Command | Description | Permission |
|---------|-------------|------------|
| `/register <path> [name]` | Register a project directory to the current channel | MANAGE_CHANNELS |
| `/unregister` | Unregister the project from the current channel | MANAGE_CHANNELS |
| `/new-project <path> [name]` | Create a new channel + thread for a project | owner only |
| `/list [channel]` | List active sessions for a channel | MANAGE_CHANNELS |
| `/del [thread_id]` | Delete a session (defaults to current thread) | MANAGE_CHANNELS |
| `/stop` | Stop the current session's Claude Code process | turn starter or owner |
| `/status [thread_id]` | Show session status (defaults to current thread) | MANAGE_CHANNELS |
| `/sessions` | Show global session overview (count, idle time, status) | MANAGE_CHANNELS |
| `/skill <name> [args]` | Send a slash command (e.g. `/commit`) to the Claude Code session | all members |
| `/branch [context]` | Fork the current session into a new thread with optional context | owner only |

### Session Reset

Type `/clear` or `/new` as a regular message in a thread to reset the session. A confirmation prompt with buttons will appear — click **Confirm** to reset the Claude Code process and start fresh, or **Cancel** to keep the current session.

### Recall

Right-click a message in a thread → **Apps** → **Recall** to recall a queued message that hasn't been delivered to Claude Code yet. If the message has already been sent, recall is not possible.

### Chatting with Claude Code

1. Run `/register /path/to/your/project` in any channel
2. Start a thread in that channel — each thread is its own Claude Code session
3. Send messages in the thread to chat with Claude Code
4. When Claude Code requests tool permissions, respond with the Discord buttons:
   - **Allow** — allow this once
   - **Always Allow** — add to the always-allowed list for this session
   - **Deny** — deny the tool call

### Reply Context

Reply to any message in the thread — pidory will extract the replied-to message content and inject it as context into your prompt so Claude Code can see what you're referring to.

### File Attachments

**Uploading files to Claude Code**: Attach files to your Discord message. pidory downloads them to the project directory and includes their paths in the message sent to Claude Code.

**Receiving files from Claude Code**: When Claude Code sends files back (e.g. images, exports), they appear as Discord file attachments on the bot's message.

### Interactive Questions

When Claude Code asks a question (via `AskUserQuestion`), pidory renders it as an interactive UI:

- **2–5 options** → Discord buttons + a free-text button for custom input
- **6–25 options** → Select menu + a free-text button
- **Free-text only** → A button that opens a text input modal

For multi-part questions, all answers are collected before being sent back to Claude Code.

### Progress Indicator

When Claude Code runs a long tool operation, pidory shows a progress indicator message that updates in real-time, showing which tool is currently executing. The indicator pauses when a permission prompt is pending and resumes when resolved.

## Configuration

`config.toml` fields (see `config.toml.example`):

### [discord]

| Field | Description | Default |
|-------|-------------|---------|
| `guild_id` | Your Discord server ID | *required* |
| `owner_id` | Your Discord user ID (bot owner) | *required* |
| `token_env` | Environment variable name for the Discord token | `"PIDORY_DISCORD_TOKEN"` |
| `notification_channel_id` | Channel ID for rate limit alerts (optional) | — |
| `project_roots` | Root directories for path autocomplete in `/register` | `[]` |
| `default_category_id` | Default category for `/new-project` channels (optional) | — |

### [claude]

| Field | Description | Default |
|-------|-------------|---------|
| `binary_path` | Path to the `claude` CLI binary | `"claude"` |
| `default_disallowed_tools` | Tools to block by default for new sessions | `[]` |
| `subprocess_timeout_secs` | Max time per Claude Code subprocess (seconds) | `600` |
| `max_sessions` | Max concurrent sessions | `10` |
| `idle_timeout_secs` | Idle session timeout in seconds (0 to disable) | `7200` |

### [database]

> **Deprecated.** `DATABASE_URL` environment variable is the authoritative database configuration source. The `path` field below is kept for backwards compatibility only and is ignored at runtime.

| Field | Description | Default |
|-------|-------------|---------|
| `path` | ~~SQLite database file path~~ (deprecated, ignored) | `"pidory.db"` |

Set `DATABASE_URL` via `/etc/pidory/db.env` (Linux service) or as an environment variable (manual/dev).

### [response]

| Field | Description | Default |
|-------|-------------|---------|
| `max_chunk_length` | Max characters per Discord message | `1900` |
| `max_chunks` | Number of chunks before falling back to file attachment | `10` |

### [ratelimit]

| Field | Description | Default |
|-------|-------------|---------|
| `update_interval_secs` | Bot presence update interval (seconds) | `60` |
| `alert_thresholds` | 5h usage % thresholds that trigger alerts | `[50, 80]` |

### [attachment]

| Field | Description | Default |
|-------|-------------|---------|
| `max_file_size_mb` | Max file size per attachment (MB) | `25` |
| `max_aggregate_size_mb` | Max total attachment size per message (MB) | `50` |
| `download_timeout_secs` | File download timeout (seconds) | `30` |

### language

| Field | Description | Default |
|-------|-------------|---------|
| `language` | UI language: `"ko"` or `"en"` | `"ko"` |

The Discord token is read from the `PIDORY_DISCORD_TOKEN` environment variable (or `.env` file) — never put it in `config.toml`.

## Rate Limit Monitoring

pidory displays Claude Code's API rate limit usage as a Discord bot presence (e.g. `Watching 5h: 42%(1h30m) | 7d: 38%`) and sends alerts when thresholds are exceeded. Rate limit data is read from Claude Code's `stream-json` output during active sessions.

### Alert Setup

To receive threshold alerts in a specific channel, set `notification_channel_id` under `[discord]` in `config.toml`.

```toml
[discord]
notification_channel_id = "123456789012345678"

[ratelimit]
alert_thresholds = [50, 80]
```

When 5-hour usage reaches a configured threshold, an alert is posted to the notification channel.

## License

MIT
