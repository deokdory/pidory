# pidory

<p align="center">
  <img src="assets/pidory.png" width="256" alt="pidory">
</p>

[English](README.md) | **한국어**

Discord ↔ Claude Code CLI 브릿지. Discord 스레드에서 메시지를 보내면 Claude Code가 응답하며, 도구 권한 요청은 버튼으로 처리하고 긴 출력은 자동으로 분할하거나 파일로 첨부합니다.

## 기능

- 스레드 기반 대화 — 각 스레드가 독립적인 Claude Code 세션에 매핑
- 장기 실행 프로세스 + 메시지 큐 + 실행 중 메시지 주입
- Discord 버튼으로 도구 권한 승인/거부 (Allow / Always Allow / Deny)
- 실시간 중간 상태 표시
- Discord 2000자 제한에 맞춘 코드 블록 인식 메시지 분할
- Rate limit 모니터링 — 봇 상태 메시지로 사용률(%) 표시, 임계값 초과 시 알림
- 세션 생명주기 관리 — 최대 세션 도달 시 LRU 자동 교체, 유휴 세션 타임아웃 정리
- 스트리밍 중간 메시지 알림 억제로 알림 스팸 방지

## 보안 모델

pidory 는 Discord 의 내장 권한 시스템에 위임하며, 세션은 **스레드 단위로 공유**됩니다.

- 스레드가 등록된 채널에 접근 가능한 사용자는 누구나 봇을 사용할 수 있습니다 (채널 VIEW_CHANNEL / SEND_MESSAGES 권한이 게이트).
- 같은 스레드의 사용자들은 **같은 Claude Code 세션을 공유**합니다. 이는 다음을 의미합니다:
  - 한 사용자가 `Always Allow` 한 도구 권한은 **그 세션 전체** 에 적용되어, 이후 같은 스레드의 다른 사용자 메시지에서도 자동 허용됩니다.
  - `/skill` 은 모든 멤버가 호출 가능하며, 임의의 Claude Code skill 을 세션에 실행할 수 있습니다.
- 관리 커맨드 (`/register`, `/unregister`, `/del`, `/status`, `/list`, `/sessions`) 는 각각 `MANAGE_GUILD` 또는 `MANAGE_CHANNELS` 권한을 요구합니다.
- `/stop` 은 해당 turn 을 시작한 사용자(또는 `owner_id`) 만 호출 가능합니다.

**⚠️ 중요**: 이 모델은 길드에 초대된 사용자들이 서로 **신뢰 관계** 라는 가정 위에 작동합니다. pidory 를 실행 중인 서버에는 **신뢰할 수 있는 사용자만 초대**하십시오. 그렇지 않으면 악의적인 사용자가 봇으로 임의 코드 실행 / 파일 조작 / 다른 사용자의 권한 승격을 할 수 있습니다.

## 사전 준비

- Rust 1.85+ (2024 edition)
- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) — Anthropic Max 구독 필요
- Discord Bot Token
- Linux 또는 macOS

## 빠른 시작

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

# 또는 서비스로 등록
./deploy/install.sh   # Linux (systemd) / macOS (launchd) 자동 감지
```

## 서비스 배포

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

`install.sh`는 릴리즈 바이너리를 빌드하고, config가 없으면 `config.toml.example`을 복사하며, 서비스 파일을 설치하고 부팅 시 자동 시작을 등록합니다.

## 사용법

### Slash 커맨드

모든 커맨드는 `config.toml`의 `owner_id`로 설정된 소유자만 사용할 수 있습니다.

| 커맨드 | 설명 |
|--------|------|
| `/register <path> [name]` | 현재 채널에 프로젝트 디렉토리 등록 |
| `/unregister` | 현재 채널의 프로젝트 등록 해제 |
| `/list [channel]` | 채널의 활성 세션 목록 조회 |
| `/del [thread_id]` | 세션 삭제 (기본값: 현재 스레드) |
| `/stop` | 현재 세션의 Claude Code 프로세스 중단 |
| `/status [thread_id]` | 세션 상태 확인 (기본값: 현재 스레드) |
| `/skill <name>` | Claude Code 세션에 슬래시 커맨드(예: `/commit`) 전송 |
| /sessions | 전역 세션 현황 조회 (세션 수, 유휴 시간, 상태) |

### Claude Code와 대화하기

1. 채널에서 `/register /path/to/your/project` 실행
2. 해당 채널에서 스레드 생성 — 각 스레드가 독립 Claude Code 세션
3. 스레드에서 메시지를 보내면 Claude Code가 응답
4. Claude Code가 도구 권한을 요청하면 버튼으로 응답:
   - **Allow** — 이번 한 번만 허용
   - **Always Allow** — 이 세션에서 항상 허용
   - **Deny** — 거부

## 설정

`config.toml` 필드 설명 (`config.toml.example` 참고):

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `discord.guild_id` | Discord 서버 ID | — |
| `discord.owner_id` | 봇 소유자 Discord 사용자 ID | — |
| `claude.binary_path` | `claude` CLI 바이너리 경로 | `"claude"` |
| `claude.default_disallowed_tools` | 기본 차단 도구 목록 | `[]` |
| `claude.subprocess_timeout_secs` | Claude Code 서브프로세스 최대 실행 시간 | `600` |
| `claude.max_sessions` | 최대 동시 세션 수 | `10` |
| `claude.idle_timeout_secs` | 유휴 세션 타임아웃 (초, 0이면 비활성화) | `7200` |
| `discord.notification_channel_id` | Rate limit 알림을 보낼 채널 ID (선택) | — |
| `response.max_chunk_length` | Discord 메시지 최대 문자 수 | `1900` |
| `response.max_chunks` | 파일 첨부로 전환하기 전 최대 청크 수 | `10` |
| `ratelimit.file_path` | Rate limit JSON 파일 경로 (선택) | — |
| `ratelimit.update_interval_secs` | Rate limit 파일 읽기 주기 | `60` |
| `ratelimit.alert_thresholds` | 5h 사용률(%) 알림 임계값 | `[50, 80]` |

Discord 토큰은 `PIDORY_DISCORD_TOKEN` 환경변수(또는 `.env` 파일)로 설정합니다. `config.toml`에 직접 넣지 마세요.

## Rate Limit 모니터링

pidory는 Claude Code의 API rate limit 사용률을 Discord 봇 상태 메시지로 표시하고 (예: `Watching 5h: 42%(1h30m) | 7d: 38%`), 임계값 초과 시 알림을 보낼 수 있습니다.

### 동작 원리

```
Claude Code statusLine 훅 → /tmp/pidory-ratelimits.json 기록
pidory가 주기적으로 파일 읽기 → 봇 상태 업데이트 + 알림 전송
```

Claude Code의 `statusLine`은 stdin으로 rate limit 데이터를 JSON으로 받습니다. 헬퍼 스크립트가 사용률을 추출해 pidory가 모니터링하는 파일에 기록합니다.

### 설정 방법

1. Claude Code statusLine 스크립트(`~/.claude/settings.json`)에 ratelimit writer 추가:

```bash
# statusLine 스크립트에서 stdin 읽은 후:
input=$(cat)
echo "$input" | bash /path/to/pidory/scripts/statusline-ratelimit-writer.sh 2>/dev/null
# ... 나머지 statusLine 스크립트
```

2. `config.toml`에서 모니터링 활성화:

```toml
[ratelimit]
file_path = "/tmp/pidory-ratelimits.json"
# update_interval_secs = 60
# alert_thresholds = [50, 80]
```

3. `[discord]` 아래에 `notification_channel_id`를 설정하면 임계값 초과 시 해당 채널로 알림을 받을 수 있습니다.

## 라이선스

MIT
