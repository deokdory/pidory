# Installing pidory

pidory is a Discord bot that bridges Discord threads to Claude Code CLI sessions.
This guide covers installation from source on Linux (systemd) and macOS (launchd).

---

## Prerequisites

Before you begin, ensure the following are available on your system:

- **Rust 1.85+** — install via [rustup](https://rustup.rs/)
- **PostgreSQL 14+** — must be reachable from the host running pidory
- **Claude Code CLI** — the `claude` binary must be in `PATH` or its path set in `config.toml`
- **Discord bot token** — obtained from the [Discord Developer Portal](https://discord.com/developers/applications)
  - Enable the following Privileged Gateway Intents: **MESSAGE CONTENT**, **GUILD MESSAGES**, **GUILD MEMBERS**
  - Invite the bot to your server with `bot` + `applications.commands` scopes
- **Linux (systemd)** or **macOS (launchd)** — other platforms are not supported by the service installer
- `openssl` and `python3` (used by `postgres-setup.sh`)

---

## Quick Start

For an existing PostgreSQL installation on a fresh Linux host:

```bash
# 1. Clone the repository
git clone https://github.com/deokdory/pidory.git
cd pidory

# 2. Set up PostgreSQL (creates role, database, and /etc/pidory/db.env)
sudo bash scripts/postgres-setup.sh

# 3. Write your Discord token
echo 'PIDORY_DISCORD_TOKEN=your_token_here' > .env

# 4. Install binaries and service
bash deploy/install.sh

# 5. Edit config.toml (guild_id, owner_id, binary_path)
$EDITOR config.toml

# 6. Start the service (Linux)
sudo systemctl start pidory
```

See [Detailed Setup](#detailed-setup) below for a step-by-step walkthrough of each stage.

---

## Detailed Setup

### 3.1 Discord Bot Creation

1. Go to [Discord Developer Portal](https://discord.com/developers/applications) and create a new application.
2. Navigate to **Bot** tab → click **Add Bot**.
3. Under **Token**, click **Reset Token** and copy the value — this is your `PIDORY_DISCORD_TOKEN`.
4. Under **Privileged Gateway Intents**, enable:
   - **SERVER MEMBERS INTENT**
   - **MESSAGE CONTENT INTENT**
5. Under **OAuth2 → URL Generator**, select scopes: `bot`, `applications.commands`.
   Select bot permissions: **Read Messages/View Channels**, **Send Messages**, **Add Reactions**, **Manage Messages**, **Embed Links**, **Attach Files**.
6. Use the generated URL to invite the bot to your server.
7. Note your server's **Guild ID** (right-click server icon → Copy Server ID — requires Developer Mode).

### 3.2 PostgreSQL Setup

Run the automated setup script as root. This installs PostgreSQL if needed, creates the `pidory` role and database, and writes `DATABASE_URL` to `/etc/pidory/db.env` (mode `0640`):

```bash
sudo bash scripts/postgres-setup.sh
```

> **Note:** This script requires Linux with systemd. macOS users must set up PostgreSQL manually and export `DATABASE_URL` in their environment before running `deploy/install.sh`.

The script performs the following steps automatically:
1. Installs `postgresql-17` (falls back to `postgresql` if 17 is unavailable)
2. Enables and starts the PostgreSQL service
3. Creates the `pidory` role with a randomly generated password
4. Creates the `pidory` database owned by the `pidory` role
5. Writes `DATABASE_URL=postgres://pidory:<password>@localhost/pidory` to `/etc/pidory/db.env`
6. Restarts `pidory.service` if already installed

To use a specific password instead of a generated one:
```bash
PIDORY_DB_PASSWORD=my_password sudo bash scripts/postgres-setup.sh
```

### 3.3 Repository and Environment

```bash
# Clone
git clone https://github.com/deokdory/pidory.git
cd pidory

# Copy example config
cp config.toml.example config.toml

# Write Discord token
echo 'PIDORY_DISCORD_TOKEN=your_token_here' > .env
```

Edit `config.toml` to set at minimum:
- `[discord] guild_id` — your Discord server ID
- `[discord] owner_id` — your Discord user ID

### 3.4 Run the Installer

```bash
bash deploy/install.sh
```

The installer performs six steps:
1. **Build** — `cargo build --release` + `cargo build --bin pidory-migrate --features migrate --release`
2. **Check .env** — warns if `PIDORY_DISCORD_TOKEN` is missing
3. **Initialize config.toml** — copies from example if absent; auto-detects `claude` binary path
4. **Install pidory-migrate** — copies to `/usr/local/bin/pidory-migrate`; creates `/etc/pidory/` directory
5. **Install skills** — copies `skills/` to `~/.claude/skills/`
6. **Install service** — writes and enables the systemd unit (Linux) or launchd plist (macOS)

> **Before running**, ensure `scripts/postgres-setup.sh` has already written `/etc/pidory/db.env`.
> On macOS, set `DATABASE_URL` as an environment variable before running the installer — the plist does not include it by default.

### 3.5 Start the Service

**Linux (systemd):**
```bash
sudo systemctl start pidory
# Optional: check it started cleanly
sudo systemctl status pidory
```

The service is already enabled at boot by the installer (`sudo systemctl enable pidory`).

**macOS (launchd):**
```bash
launchctl load ~/Library/LaunchAgents/com.pidory.bot.plist
# Or using bootstrap (macOS 10.15+):
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/com.pidory.bot.plist
```

---

## Verification

### Linux

```bash
# Service status
sudo systemctl status pidory

# Live logs
journalctl -u pidory -f

# Check the last 50 log lines
journalctl -u pidory -n 50 --no-pager
```

### macOS

```bash
# Service status
launchctl list | grep pidory

# Live logs
tail -f ~/.pidory/stderr.log
```

### Discord

Once the service is running:
1. Check that the bot appears **online** in your Discord server.
2. Use `/status` (slash command) to confirm the bot responds.

---

## Configuration

`config.toml` is the main configuration file. The three fields most commonly requiring changes:

| Field | Section | Description |
|---|---|---|
| `guild_id` | `[discord]` | Your Discord server (guild) ID |
| `owner_id` | `[discord]` | Your Discord user ID (commands restricted to owner) |
| `binary_path` | `[claude]` | Absolute path to the `claude` CLI binary |

For the full set of options, see the **Configuration** section of README.md.

### Environment Variables

All environment variables recognized by pidory:

| Variable | Required | Source | Description |
|---|---|---|---|
| `PIDORY_DISCORD_TOKEN` | Yes | `.env` file or shell env | Discord bot token |
| `DATABASE_URL` | Yes | `/etc/pidory/db.env` (Linux) or shell env | PostgreSQL connection URL |
| `PIDORY_CONFIG` | No | Injected by service file | Path to `config.toml`; defaults to `./config.toml` |
| `PIDORY_LOCALE` | No | Shell env | Override bot message language (`ko` or `en`; default `ko`) |
| `RUST_LOG` | No | Injected by service file | Log level filter (e.g. `info`, `debug`, `warn`) |

On Linux, the systemd unit loads `.env` and `/etc/pidory/db.env` automatically via `EnvironmentFile`. On macOS, only `PIDORY_DISCORD_TOKEN`, `PIDORY_CONFIG`, and `RUST_LOG` are injected by the plist; `DATABASE_URL` must be set separately.

---

## Updating

### Automatic update (Linux, via slash command)

The `/update` slash command triggers a supervised in-place update from within Discord. Before applying the update it:
1. Verifies `DATABASE_URL` is set and reachable
2. Verifies `/usr/local/bin/pidory-migrate` exists and is executable
3. Takes an automatic `pg_dump` backup of the database
4. Builds and installs the new binary
5. Restarts the service

Use `/update` when you want a safe, logged, single-command upgrade.

### Manual update

```bash
cd /path/to/pidory
git pull
cargo build --release
cargo build --bin pidory-migrate --features migrate --release
sudo install -o "$(whoami)" -m 0755 target/release/pidory-migrate /usr/local/bin/pidory-migrate
sudo systemctl restart pidory
```

The service runs `pidory-migrate` as `ExecStartPre`, so database migrations are applied automatically on restart.

---

## Troubleshooting

### `permission denied: /etc/pidory/db.env`

The service user does not have read access to the env file. Fix:

```bash
sudo chown root:$(whoami) /etc/pidory/db.env
sudo chmod 0640 /etc/pidory/db.env
```

### `DATABASE_URL` is invalid or not set

Verify the connection URL is reachable:

```bash
psql "$DATABASE_URL" -c 'SELECT 1'
```

If this fails, re-run `sudo bash scripts/postgres-setup.sh` to regenerate `/etc/pidory/db.env`.

### Service fails to start

```bash
# Inspect the last 50 log lines
sudo journalctl -u pidory -n 50 --no-pager
```

Common causes:
- `/usr/local/bin/pidory-migrate` missing or not executable — re-run `bash deploy/install.sh`
- `DATABASE_URL` not set — check `/etc/pidory/db.env` exists and is loaded by the unit
- `PIDORY_DISCORD_TOKEN` missing — check `.env` in the project directory

### Migrating from SQLite (v0.6.x → v0.7.0+)

v0.7.0 is a breaking change: the database backend switched from SQLite to PostgreSQL. Automatic migration of existing SQLite data is not supported by the installer. Refer to the [v0.7.0 release notes](https://github.com/deokdory/pidory/releases/tag/v0.7.0) for the manual migration procedure.

### Discord bot appears offline

- Verify the `PIDORY_DISCORD_TOKEN` value matches the token in the Developer Portal (tokens reset after regeneration).
- Confirm **Message Content Intent** and **Server Members Intent** are enabled in the Bot tab.
- Confirm `guild_id` in `config.toml` matches your server's ID.
- Check logs: `journalctl -u pidory -n 50 --no-pager` (Linux) or `tail ~/.pidory/stderr.log` (macOS).
