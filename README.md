# pidory

<p align="center">
  <img src="assets/pidory.png" width="256" alt="pidory">
</p>

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://blog.rust-lang.org/2024/10/17/Rust-2024-edition.html)
[![Version](https://img.shields.io/badge/version-v0.7.0-green.svg)](https://github.com/deokdory/pidory/releases)

**English** | [한국어](./README.ko.md)

## Overview

pidory is a Discord bot that bridges Discord threads to [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI sessions via `stream-json` IPC. Each Discord thread maps to a long-lived Claude Code subprocess — tool permission prompts appear as interactive buttons, file attachments flow bidirectionally, multi-user sender prefixes keep context clear, and rate limit usage is surfaced as bot presence.

**Core values**

- **Per-thread sessions** — isolated Claude Code subprocess per Discord thread, no cross-contamination
- **Permission buttons** — Allow / Always Allow / Deny rendered as Discord buttons
- **Bidirectional file attachments** — upload files to Claude Code; receive files back in Discord
- **Multi-user aware** — sender prefix injected into every message so Claude knows who is talking

## Features

- **Thread-based sessions** — each thread maps to an independent Claude Code subprocess
- **Tool permissions** — Allow / Always Allow / Deny via Discord buttons
- **File attachments** — upload files to Claude Code from Discord; receive generated files back
- **Multi-user sender prefix** — sender's display name prepended to every message in multi-user threads
- **`/update` with pre-flight validation** — bot fetches latest release, rebuilds from source, and restarts the service
- **i18n** — Korean (default) and English UI; select with `language = "ko"` / `"en"` in config
- **Rate limit monitoring** — bot presence shows 5h/7d usage %; configurable threshold alerts
- **`/branch` context fork** — duplicate a session into a new thread with optional context snapshot
- **`/sleep` session suspend** — pause a session, releasing subprocess resources while preserving thread state
- **`/skill` invocation** — call any Claude Code skill (slash command) from Discord
- **Attachment download** — files attached to Discord messages are downloaded to the project directory
- **PostgreSQL backend** — production-grade persistence; migration path from legacy SQLite included

## Prerequisites

- **Rust 1.85+** (2024 edition)
- **PostgreSQL 14+** (17 recommended) — `scripts/postgres-setup.sh` handles installation on Linux
- **[Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code)** — requires an Anthropic Max subscription
- **Discord Bot Token** — with `MESSAGE CONTENT INTENT` enabled
- **Linux** (systemd) or **macOS** (launchd)

## Install

See [INSTALL.md](./INSTALL.md) for step-by-step setup including Discord bot creation, PostgreSQL configuration, and service deployment.

**Quick path (Linux):**

```bash
git clone https://github.com/deokdory/pidory.git && cd pidory
echo 'PIDORY_DISCORD_TOKEN=your_token_here' > .env
bash deploy/install.sh                # build + service + pidory-migrate install
sudo bash scripts/postgres-setup.sh   # PostgreSQL setup (requires install.sh complete)
$EDITOR config.toml                   # set guild_id, owner_id
sudo systemctl start pidory
```

## Configuration

`config.toml` controls all runtime behavior. Copy `config.toml.example` to `config.toml` and fill in the required fields.

### Key sections

#### [discord]

| Field | Description | Default |
|-------|-------------|---------|
| `guild_id` | Your Discord server ID | **required** |
| `owner_id` | Your Discord user ID (bot owner) | **required** |
| `token_env` | Env var name for the Discord token | `"PIDORY_DISCORD_TOKEN"` |
| `notification_channel_id` | Channel ID for rate limit alerts | — |
| `project_roots` | Root dirs for path autocomplete in `/register` | `[]` |
| `default_category_id` | Default category for `/new-project` channels | — |

#### [claude]

| Field | Description | Default |
|-------|-------------|---------|
| `binary_path` | Path to the `claude` CLI binary | `"claude"` |
| `default_disallowed_tools` | Tools blocked by default for new sessions | `[]` |
| `subprocess_timeout_secs` | Max subprocess runtime (seconds) | `600` |
| `max_sessions` | Max concurrent sessions | `10` |
| `idle_timeout_secs` | Idle session timeout in seconds (0 = disabled) | `7200` |

#### [response]

| Field | Description | Default |
|-------|-------------|---------|
| `max_chunk_length` | Max characters per Discord message | `1900` |
| `max_chunks` | Chunks before falling back to file attachment | `10` |

#### [attachment]

| Field | Description | Default |
|-------|-------------|---------|
| `max_file_size_mb` | Max file size per attachment (MB) | `25` |
| `max_aggregate_size_mb` | Max total attachment size per message (MB) | `50` |
| `download_timeout_secs` | File download timeout (seconds) | `30` |

#### [ratelimit]

| Field | Description | Default |
|-------|-------------|---------|
| `update_interval_secs` | Bot presence update interval (seconds) | `60` |
| `alert_thresholds` | 5h usage % thresholds for alerts | `[50, 80]` |

### Environment variables

| Variable | Description |
|----------|-------------|
| `PIDORY_DISCORD_TOKEN` | Discord bot token — **never put this in config.toml** |
| `DATABASE_URL` | PostgreSQL connection string (authoritative source) |
| `PIDORY_CONFIG` | Path to config.toml (default: `./config.toml`) |
| `PIDORY_LOCALE` | Override UI locale (`ko` or `en`) |
| `RUST_LOG` | Log filter (e.g. `pidory=debug,warn`) |

> **Note:** `[database] path` in `config.toml` is **deprecated** and ignored at runtime. Use `DATABASE_URL` exclusively.

## Usage

### Slash Commands

| Command | Description | Permission |
|---------|-------------|------------|
| `/register <path> [name]` | Register a project directory to the current channel | MANAGE_CHANNELS |
| `/unregister` | Unregister the project from the current channel | MANAGE_CHANNELS |
| `/new-project <path> [name]` | Create a new channel + thread for a project | owner only |
| `/list [channel]` | List active sessions for a channel | MANAGE_CHANNELS |
| `/status [thread_id]` | Show session status | MANAGE_CHANNELS |
| `/sessions` | Global session overview (count, idle time, status) | MANAGE_CHANNELS |
| `/del [thread_id]` | Delete a session (defaults to current thread) | MANAGE_CHANNELS |
| `/stop` | Stop the current turn's Claude Code process | turn starter or owner |
| `/skill <name> [args]` | Send a skill (e.g. `/commit`) to the Claude Code session | all members |
| `/recall` | Recall a queued message before it reaches Claude Code | turn starter |
| `/branch [context]` | Fork session into a new thread with optional context | owner only |
| `/model <model_name>` | Switch Claude model for the current session | all members |
| `/sleep` | Suspend the session (release subprocess, preserve thread) | all members |
| `/update` | Pull latest release, rebuild from source, and restart the service | owner only |

### Chatting with Claude Code

1. Run `/register /path/to/project` in any channel
2. Open a thread in that channel — each thread is an independent Claude Code session
3. Send messages to interact with Claude Code
4. When a tool permission prompt appears, click a button:
   - **Allow** — allow this tool call once
   - **Always Allow** — add to the always-allowed list for this session
   - **Deny** — deny the tool call

### Session Reset

Send `/clear` or `/new` as a plain message in a thread to reset the session. A confirmation prompt appears — click **Confirm** to restart, or **Cancel** to keep the current session.

### File Attachments

Attach files to a Discord message — pidory downloads them to the project directory and passes their paths to Claude Code. When Claude Code produces output files, they appear as Discord file attachments.

### Reply Context

Reply to any message in a thread — pidory extracts the replied-to content and injects it as context into your prompt.

## Permission Model

pidory surfaces Claude Code's permission prompts as Discord buttons: **Allow**, **Always Allow**, and **Deny**.

- **Allow** — grants the tool call for this request only
- **Always Allow** — adds the tool to the session's always-allowed list; subsequent requests for the same tool are auto-approved without prompting
- **Deny** — rejects the tool call

Permission buttons are restricted to the user who started the current turn (or the `owner_id`).

### ⚠️ Multi-user beta — Always Allow affects all users in the thread

Sessions are shared per thread. When one user clicks **Always Allow**, that permission applies to the **entire session** — every subsequent message from any user in the same thread will auto-approve that tool. Only add pidory to servers where all participants genuinely trust each other. A malicious user could exploit Always Allow to execute arbitrary code, manipulate files, or escalate permissions on behalf of other users.

## Upgrading

### Using `/update` (recommended)

The `/update` slash command (owner only) performs a guided in-place update:

1. Verifies `DATABASE_URL` is set and reachable
2. Takes an automatic `pg_dump` backup of the current database
3. Runs `git fetch` and resets the worktree to the latest release tag
4. Rebuilds the binary with `cargo build --release`
5. Schedules a delayed service restart (~30 s) so the response message can be delivered

For the full `/update` pre-flight checks and rollback behavior, see `INSTALL.md` → "Updating".

### Manual update

See [INSTALL.md → Updating](./INSTALL.md#updating) for the manual command sequence.

### SQLite → PostgreSQL migration (v0.7.0 breaking change)

v0.7.0 drops SQLite support entirely. If you are upgrading from v0.6.x, follow the migration guide in [`docs/release-notes/v0.7.0.md`](./docs/release-notes/v0.7.0.md).

The `pidory-migrate` binary (installed to `/usr/local/bin/` by `deploy/install.sh`) handles the one-time data import:

```bash
# Run automatically as ExecStartPre when the service starts on an empty PostgreSQL DB
# To run manually:
pidory-migrate
```

Migration is roll-forward only — there is no path back to SQLite.

## Architecture

### Data flow

```
Discord message
  → handler::message
    → SessionManager::send_message (mpsc queue, per-thread worker)
      → Claude CLI subprocess (stream-json stdin/stdout)
        → parser::parse_line → StreamEvent
  → handler::message ← event_rx (mpsc channel)
→ Discord response
```

### Module structure

| Module | Responsibility |
|--------|----------------|
| `subprocess/session_manager.rs` | Spawns Claude CLI; one worker task per thread. Handles stdin writes, stdout parsing, mid-turn injection, permission flow. |
| `subprocess/parser/` | Parses JSON lines → `StreamEvent` (Init, Assistant, User, RateLimit, Result, ControlRequest, …); split across `raw.rs`, `events.rs`, etc. |
| `subprocess/permission.rs` | `PermissionCache` (per-session Always Allow set); `PermissionRequest`/`PermissionDecision` via oneshot channels |
| `handler/message/` | Routes Discord events to session queues; 500 ms fast-complete check; streams events to Discord; split across `mod.rs`, `event_processor.rs`, `interaction.rs`, etc. |
| `handler/formatter.rs` | Code-block-aware message splitting at 2000-char limit |
| `handler/permission_ui.rs` | Builds Allow/Always Allow/Deny button messages; parses `perm:{id}:{action}` custom IDs |
| `handler/status.rs` | `StatusMessage` — single editable Discord message with tool history (1.5 s rate limit) |
| `commands/` | Slash commands — all declared with poise, guild-scoped |
| `db/` | PostgreSQL via sqlx; compile-time checked migrations; atomic session locking via CAS on status column |
| `i18n/` | Korean / English message catalog; runtime locale selection |

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). Issues and PRs are welcome — please open an issue first to discuss significant changes.

## License

pidory is licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for the full text.

## Acknowledgements

- [Anthropic](https://www.anthropic.com) — Claude Code CLI and the stream-json protocol
- [poise](https://github.com/serenity-rs/poise) / [serenity](https://github.com/serenity-rs/serenity) — Discord framework for Rust
- [sqlx](https://github.com/launchbadge/sqlx) — async PostgreSQL driver with compile-time checked queries
- [tokio](https://tokio.rs) — async runtime powering the concurrent session worker model
