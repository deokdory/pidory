# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is pidory

Discord bot that bridges Discord threads to Claude Code CLI sessions via `stream-json` IPC. Each Discord thread maps to a long-lived Claude Code subprocess; tool permission prompts (Allow/Always Allow/Deny) are surfaced as Discord buttons.

## Build & Run

```bash
cargo build                    # dev build
cargo run --release            # release run (needs config.toml + PIDORY_DISCORD_TOKEN + DATABASE_URL)
cargo test                     # all tests (unit only, no integration tests)
cargo test -- --test-threads=1 # if tests conflict
```

Single test: `cargo test <test_name>` (e.g., `cargo test parse_control_request`)

Environment: Rust 2024 edition, stable toolchain. Key deps: poise 0.6 (Discord framework on serenity 0.12), sqlx 0.8 (PostgreSQL), tokio.

`DATABASE_URL` env is required at runtime (e.g. `postgres://pidory:<pw>@localhost/pidory`). On Linux service deployments this comes from `/etc/pidory/db.env` via systemd `EnvironmentFile`.

## Architecture

### Data flow

```
Discord message → handler::message → SessionManager::send_message (mpsc queue)
                                      ↕ worker task (tokio::spawn per session)
                                      Claude CLI subprocess (stream-json stdin/stdout)
                                      ↕ StreamEvent parsed by parser::parse_line
Discord ← handler::message ← event_rx (mpsc channel)
```

### Module layout

- **`subprocess/`** — Claude CLI process lifecycle
  - `session_manager.rs` — spawns `claude` with `--input-format stream-json --output-format stream-json --permission-prompt-tool stdio`. One worker task per session handles stdin writes, stdout parsing, mid-turn message injection, and permission flow. Sessions keyed by Discord thread_id.
  - `parser.rs` — parses JSON lines from Claude CLI stdout into `StreamEvent` enum (Init, Assistant, User, RateLimit, Result, UserReplay, ControlRequest). Also builds `control_response` JSON for allow/deny.
  - `permission.rs` — `PermissionCache` (per-session "Always Allow" set) and `PermissionRequest`/`PermissionDecision` types bridging worker ↔ handler via oneshot channels.

- **`handler/`** — Discord event processing
  - `message.rs` — core event handler. Routes `FullEvent::Message` to session queue and `FullEvent::InteractionCreate` (button clicks) to permission resolution. `process_turn_events` does a 500ms fast-complete check: if the turn finishes quickly, batches the response; otherwise streams events to Discord in real-time.
  - `formatter.rs` — formats tool calls/results for Discord. `split_message` does code-block-aware splitting at the 2000-char limit.
  - `permission_ui.rs` — builds Discord button messages (Allow/Always Allow/Deny) for `control_request` events. `parse_permission_custom_id` parses `perm:{request_id}:{action}` button IDs.
  - `status.rs` — `StatusMessage` that edits a single Discord message with tool history (rate-limited to 1.5s between edits).
  - `emoji.rs` — reaction state machine (Running/Done/Error/Timeout) on the triggering user message.

- **`commands/`** — Discord slash commands (all `owners_only`)
  - `register.rs` — `/register <path>` and `/unregister` for channel → project mapping.
  - `session.rs` — `/list`, `/del`, `/status` for session management.
  - `skill.rs` — `/skill <name>` sends `/<skill_name>` to the Claude CLI session. Loads descriptions from `~/.claude/skills/` for autocomplete.

- **`db/`** — PostgreSQL via sqlx with compile-time checked migrations
  - Two tables: `projects` (channel_id PK → path) and `sessions` (thread_id PK → channel_id FK, session_id, status).
  - `try_acquire_session` uses atomic UPDATE to prevent concurrent turns on the same thread.
  - Migrations in `migrations/`. Pool initialized via `DATABASE_URL` env.

- **`config.rs`** — TOML config with serde defaults. Loaded from `PIDORY_CONFIG` env or `./config.toml`.

### Key concurrency patterns

- **Message queue**: Each session has an `mpsc::channel(5)` queue. Primary messages carry an `event_tx` for streaming results back; mid-turn injected messages have `event_tx: None` and are written to stdin without waiting for a result.
- **Permission flow**: `control_request` from Claude CLI → `PermissionRequest` sent via mpsc → handler creates Discord buttons → button click sends `PermissionDecision` via oneshot → worker writes `control_response` to stdin. `PermissionCache` auto-allows previously "Always Allow"ed tools.
- **Session status locking**: PostgreSQL `try_acquire_session` (atomic CAS on status column) prevents race conditions when multiple messages arrive for the same thread.

## Configuration

`config.toml` — see `config.toml.example`. Discord token via `PIDORY_DISCORD_TOKEN` env var (or `.env` file).

`DATABASE_URL` env is the authoritative database source. `config.toml`'s `[database] path` field is **deprecated** — kept for backwards compatibility, ignored at runtime. On Linux service deployments, `DATABASE_URL` is injected from `/etc/pidory/db.env`.

## Privacy / PII Forwarding

멀티유저 스레드 사용 시 참여자의 Discord 식별자(server nickname, global display name, username, user ID snowflake)가 sender prefix 형태로 Claude CLI subprocess에 전달되어 Anthropic API로 송출됨. 현재 owner-only 운영이라 즉시 위험은 없지만, 다른 사용자를 스레드에 참여시키기 전 명시적 동의 / 개인정보 처리방침 검토 필요. (#316 도입)

## Deployment

`deploy/install.sh` — auto-detects Linux (systemd) / macOS (launchd). Service files in `deploy/`.
