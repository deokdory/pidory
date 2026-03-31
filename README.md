# pidory

<p align="center">
  <img src="https://github.com/user-attachments/assets/473af1d5-65b4-430e-bbf5-178f6abf0e37" width="256">
</p>

**English** | [한국어](README.ko.md)

Discord ↔ Claude Code CLI bridge. Send messages in a Discord thread and get Claude Code responses — tool permission prompts appear as interactive buttons, and long outputs are split or attached automatically.

## Features

- Thread-based conversations mapped to Claude Code sessions
- Long-lived process with message queue + mid-turn message injection
- Tool permission approve/deny with Discord buttons (Allow / Always Allow / Deny)
- Real-time intermediate status display
- Code block-aware message splitting for Discord's 2000 char limit

## Prerequisites

- Rust 1.85+ (2024 edition)
- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) — requires Anthropic Max subscription
- Discord Bot Token
- Linux (for systemd deployment)

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

# Or with systemd (Linux)
./deploy/install.sh
sudo systemctl start pidory
```

## Systemd Deployment

```bash
./deploy/install.sh
sudo systemctl start pidory
sudo systemctl status pidory
journalctl -u pidory -f
```

`install.sh` builds the release binary, copies `config.toml.example` if no config exists, installs the service file, and enables it on boot.

## Usage

### Slash Commands

All commands are owner-only (restricted to the `owner_id` set in `config.toml`).

| Command | Description |
|---------|-------------|
| `/register <path> [name]` | Register a project directory to the current channel |
| `/unregister` | Unregister the project from the current channel |
| `/list [channel]` | List active sessions for a channel |
| `/del [thread_id]` | Delete a session (defaults to current thread) |
| `/status [thread_id]` | Show session status (defaults to current thread) |

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
| `response.max_chunk_length` | Max characters per Discord message | `1900` |
| `response.max_chunks` | Chunks before falling back to file attachment | `10` |

The Discord token is read from the `PIDORY_DISCORD_TOKEN` environment variable (or `.env` file) — never put it in `config.toml`.

## License

MIT
