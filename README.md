# pidory

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

---

## 한국어 가이드 (Korean)

Discord ↔ Claude Code CLI 브릿지. Discord 스레드에서 메시지를 보내면 Claude Code가 응답하며, 도구 권한 요청은 버튼으로 처리하고 긴 출력은 자동으로 분할하거나 파일로 첨부합니다.

### 기능

- 스레드 기반 대화 — 각 스레드가 독립적인 Claude Code 세션에 매핑
- 장기 실행 프로세스 + 메시지 큐 + 실행 중 메시지 주입
- Discord 버튼으로 도구 권한 승인/거부 (Allow / Always Allow / Deny)
- 실시간 중간 상태 표시
- Discord 2000자 제한에 맞춘 코드 블록 인식 메시지 분할

### 사전 준비

- Rust 1.85+ (2024 edition)
- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) — Anthropic Max 구독 필요
- Discord Bot Token
- Linux (systemd 배포 시)

### 1. Discord 봇 생성

1. https://discord.com/developers/applications → **New Application**
2. 이름 입력 후 **Bot** 탭으로 이동
3. **Reset Token** 클릭 → 토큰 복사 (한 번만 표시됨!)
4. **Privileged Gateway Intents**에서 **MESSAGE CONTENT INTENT** 활성화
5. **OAuth2** → **URL Generator** 이동:
   - Scopes: `bot`, `applications.commands`
   - Bot Permissions: `Send Messages`, `Read Message History`, `Add Reactions`, `Manage Messages`, `Use Slash Commands`, `Embed Links`, `Attach Files`
6. 생성된 URL로 봇을 서버에 초대

### 2. Discord ID 확인

1. Discord: **설정** → **고급** → **개발자 모드** 활성화
2. 서버 아이콘 우클릭 → **서버 ID 복사** → `guild_id`
3. 내 프로필 우클릭 → **사용자 ID 복사** → `owner_id`

### 3. 클론 및 설정

```bash
git clone https://github.com/deokdory/pidory.git
cd pidory
cp config.toml.example config.toml
# config.toml 편집 — guild_id, owner_id 입력
```

### 4. Discord 토큰 설정

```bash
echo 'PIDORY_DISCORD_TOKEN=your_token_here' > .env
```

### 5. 실행

```bash
# 직접 실행
cargo run --release

# 또는 systemd로 (Linux)
./deploy/install.sh
sudo systemctl start pidory
```

### systemd 배포

```bash
./deploy/install.sh
sudo systemctl start pidory
sudo systemctl status pidory
journalctl -u pidory -f
```

`install.sh`는 릴리즈 바이너리를 빌드하고, config가 없으면 `config.toml.example`을 복사하며, 서비스 파일을 설치하고 부팅 시 자동 시작을 등록합니다.

### 사용법

#### Slash 커맨드

모든 커맨드는 `config.toml`의 `owner_id`로 설정된 소유자만 사용할 수 있습니다.

| 커맨드 | 설명 |
|--------|------|
| `/register <path> [name]` | 현재 채널에 프로젝트 디렉토리 등록 |
| `/unregister` | 현재 채널의 프로젝트 등록 해제 |
| `/list [channel]` | 채널의 활성 세션 목록 조회 |
| `/del [thread_id]` | 세션 삭제 (기본값: 현재 스레드) |
| `/status [thread_id]` | 세션 상태 확인 (기본값: 현재 스레드) |

#### Claude Code와 대화하기

1. 채널에서 `/register /path/to/your/project` 실행
2. 해당 채널에서 스레드 생성 — 각 스레드가 독립 Claude Code 세션
3. 스레드에서 메시지를 보내면 Claude Code가 응답
4. Claude Code가 도구 권한을 요청하면 버튼으로 응답:
   - **Allow** — 이번 한 번만 허용
   - **Always Allow** — 이 세션에서 항상 허용
   - **Deny** — 거부

### 설정

`config.toml` 필드 설명 (`config.toml.example` 참고):

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `discord.guild_id` | Discord 서버 ID | — |
| `discord.owner_id` | 봇 소유자 Discord 사용자 ID | — |
| `claude.binary_path` | `claude` CLI 바이너리 경로 | `"claude"` |
| `claude.default_disallowed_tools` | 기본 차단 도구 목록 | `[]` |
| `claude.subprocess_timeout_secs` | Claude Code 서브프로세스 최대 실행 시간 | `600` |
| `claude.max_sessions` | 최대 동시 세션 수 | `10` |
| `response.max_chunk_length` | Discord 메시지 최대 문자 수 | `1900` |
| `response.max_chunks` | 파일 첨부로 전환하기 전 최대 청크 수 | `10` |

Discord 토큰은 `PIDORY_DISCORD_TOKEN` 환경변수(또는 `.env` 파일)로 설정합니다. `config.toml`에 직접 넣지 마세요.

---

## License

MIT
