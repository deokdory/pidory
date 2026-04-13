# pidory

<p align="center">
  <img src="assets/pidory.png" width="256" alt="pidory">
</p>

[English](README.md) | **한국어**

Discord ↔ Claude Code CLI 브릿지. Discord 스레드에서 메시지를 보내면 Claude Code가 응답하며, 도구 권한 요청은 버튼으로 처리하고 긴 출력은 자동으로 분할하거나 파일로 첨부합니다.

## 기능

- **스레드 기반 대화** — 각 스레드가 독립적인 Claude Code 세션에 매핑
- **장기 실행 프로세스** — 메시지 큐 + 실행 중 메시지 주입
- **도구 권한 관리** — Discord 버튼으로 승인/거부 (Allow / Always Allow / Deny)
- **대화형 질문** — Claude Code의 `AskUserQuestion`을 버튼, 선택 메뉴, 텍스트 입력으로 표시
- **파일 첨부** — Discord에서 파일 업로드 → Claude Code, Claude Code에서 파일 전송 → Discord
- **답장 컨텍스트** — Discord 답장(Reply)으로 참조 메시지를 프롬프트에 포함
- **진행 상태 표시** — 오래 걸리는 도구 실행 시 실시간 진행 상황 표시
- **메시지 분할** — Discord 2000자 제한에 맞춘 코드 블록 인식 분할, 초과 시 파일 첨부로 전환
- **Rate limit 모니터링** — 봇 상태 메시지로 사용률(%) 표시, 임계값 초과 시 알림
- **세션 생명주기 관리** — 최대 세션 도달 시 LRU 자동 교체, 유휴 세션 타임아웃 정리
- **알림 억제** — 스트리밍 중간 메시지는 알림 없이 전송
- **다국어 UI** — 한국어(기본) / 영어 지원

## 보안 모델

pidory는 Discord의 내장 권한 시스템에 위임하며, 세션은 **스레드 단위로 공유**됩니다.

- 스레드가 등록된 채널에 접근 가능한 사용자는 누구나 봇을 사용할 수 있습니다 (채널 VIEW_CHANNEL / SEND_MESSAGES 권한이 게이트).
- 같은 스레드의 사용자들은 **같은 Claude Code 세션을 공유**합니다. 이는 다음을 의미합니다:
  - 한 사용자가 `Always Allow` 한 도구 권한은 **그 세션 전체**에 적용되어, 이후 같은 스레드의 다른 사용자 메시지에서도 자동 허용됩니다.
  - `/skill`은 모든 멤버가 호출 가능하며, 임의의 Claude Code skill을 세션에 실행할 수 있습니다.
- 관리 커맨드 (`/register`, `/unregister`, `/del`, `/status`, `/list`, `/sessions`)는 각각 `MANAGE_GUILD` 또는 `MANAGE_CHANNELS` 권한을 요구합니다.
- `/stop`은 해당 turn을 시작한 사용자(또는 `owner_id`)만 호출 가능합니다.
- 권한 버튼(Allow / Always Allow / Deny)은 해당 turn을 시작한 사용자(또는 `owner_id`)만 클릭 가능합니다.

**주의: 다중 사용자 지원은 아직 베타입니다.** 이 모델은 길드에 초대된 사용자들이 서로 **신뢰 관계**라는 가정 위에 작동합니다. pidory를 실행 중인 서버에는 **진짜 믿을 수 있는 사람만 초대**하고 함께 작업하세요. 그렇지 않으면 악의적인 사용자가 봇으로 임의 코드 실행 / 파일 조작 / 다른 사용자의 권한 승격을 할 수 있습니다.

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

`install.sh`는 릴리즈 바이너리를 빌드하고, config가 없으면 `config.toml.example`을 복사하며, 서비스 파일을 설치하고 부팅 시 자동 시작을 등록합니다. 내장 스킬도 `~/.claude/skills/`에 자동 배포합니다.

## 업데이트

기존 설치를 업데이트하려면:

```bash
cd pidory
./deploy/update.sh
```

수행 내용:
1. 최신 변경사항 pull (fast-forward only)
2. 릴리스 바이너리 재빌드
3. 내장 스킬을 `~/.claude/skills/`에 동기화

업데이트 후 서비스를 재시작하세요:
- **Linux**: `sudo systemctl restart pidory`
- **macOS**: `launchctl kickstart -k gui/$(id -u)/com.pidory.bot`

## 사용법

### Slash 커맨드

| 커맨드 | 설명 | 권한 |
|--------|------|------|
| `/register <path> [name]` | 현재 채널에 프로젝트 디렉토리 등록 | MANAGE_CHANNELS |
| `/unregister` | 현재 채널의 프로젝트 등록 해제 | MANAGE_CHANNELS |
| `/new-project <path> [name]` | 새 프로젝트용 채널 + 스레드 생성 | 소유자 전용 |
| `/list [channel]` | 채널의 활성 세션 목록 조회 | MANAGE_CHANNELS |
| `/del [thread_id]` | 세션 삭제 (기본값: 현재 스레드) | MANAGE_CHANNELS |
| `/stop` | 현재 세션의 Claude Code 프로세스 중단 | turn 시작자 또는 소유자 |
| `/status [thread_id]` | 세션 상태 확인 (기본값: 현재 스레드) | MANAGE_CHANNELS |
| `/sessions` | 전역 세션 현황 조회 (세션 수, 유휴 시간, 상태) | MANAGE_CHANNELS |
| `/skill <name> [args]` | Claude Code 세션에 슬래시 커맨드(예: `/commit`) 전송 | 모든 멤버 |
| `/branch [context]` | 현재 세션을 새 스레드로 분기 (컨텍스트 선택 가능) | 소유자 전용 |

### 세션 리셋

스레드에서 일반 메시지로 `/clear` 또는 `/new`를 입력하면 세션을 리셋할 수 있습니다. 확인 버튼이 표시되며, **확인**을 누르면 Claude Code 프로세스를 종료하고 새로 시작합니다. **취소**를 누르면 현재 세션이 유지됩니다.

### 메시지 회수 (Recall)

스레드의 메시지를 우클릭 → **앱** → **Recall**을 선택하면 아직 Claude Code에 전달되지 않은 대기 중인 메시지를 회수할 수 있습니다. 이미 전달된 메시지는 회수할 수 없습니다.

### Claude Code와 대화하기

1. 채널에서 `/register /path/to/your/project` 실행
2. 해당 채널에서 스레드 생성 — 각 스레드가 독립 Claude Code 세션
3. 스레드에서 메시지를 보내면 Claude Code가 응답
4. Claude Code가 도구 권한을 요청하면 버튼으로 응답:
   - **Allow** — 이번 한 번만 허용
   - **Always Allow** — 이 세션에서 항상 허용
   - **Deny** — 거부

### 답장 컨텍스트

스레드 내 메시지에 답장(Reply)하면, pidory가 원본 메시지 내용을 추출해 프롬프트에 컨텍스트로 주입합니다. Claude Code가 어떤 메시지를 참조하는지 파악할 수 있습니다.

### 파일 첨부

**Discord → Claude Code**: 메시지에 파일을 첨부하면 pidory가 프로젝트 디렉토리에 다운로드하고, 파일 경로를 Claude Code에 전달합니다.

**Claude Code → Discord**: Claude Code가 파일을 보내면 (예: 이미지, 내보내기 등) Discord 메시지에 파일 첨부로 표시됩니다.

### 대화형 질문

Claude Code가 질문을 할 때 (`AskUserQuestion`) pidory가 대화형 UI로 표시합니다:

- **2~5개 선택지** → Discord 버튼 + 자유 텍스트 입력 버튼
- **6~25개 선택지** → 선택 메뉴(Select Menu) + 자유 텍스트 입력 버튼
- **자유 텍스트만** → 텍스트 입력 모달을 여는 버튼

여러 질문이 있는 경우 모든 답변을 수집한 후 한꺼번에 Claude Code에 전송합니다.

### 진행 상태 표시

Claude Code가 오래 걸리는 도구를 실행할 때, pidory가 실시간으로 업데이트되는 진행 상태 메시지를 표시합니다. 현재 실행 중인 도구 이름이 표시되며, 권한 요청 대기 중에는 일시정지되고 해결되면 재개됩니다.

## 설정

`config.toml` 필드 설명 (`config.toml.example` 참고):

### [discord]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `guild_id` | Discord 서버 ID | *필수* |
| `owner_id` | 봇 소유자 Discord 사용자 ID | *필수* |
| `token_env` | Discord 토큰 환경변수 이름 | `"PIDORY_DISCORD_TOKEN"` |
| `notification_channel_id` | Rate limit 알림을 보낼 채널 ID (선택) | — |
| `project_roots` | `/register`에서 경로 자동완성에 사용할 루트 디렉토리 | `[]` |
| `default_category_id` | `/new-project`의 기본 카테고리 (선택) | — |

### [claude]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `binary_path` | `claude` CLI 바이너리 경로 | `"claude"` |
| `default_disallowed_tools` | 새 세션의 기본 차단 도구 목록 | `[]` |
| `subprocess_timeout_secs` | Claude Code 서브프로세스 최대 실행 시간 (초) | `600` |
| `max_sessions` | 최대 동시 세션 수 | `10` |
| `idle_timeout_secs` | 유휴 세션 타임아웃 (초, 0이면 비활성화) | `7200` |

### [database]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `path` | SQLite 데이터베이스 파일 경로 | `"pidory.db"` |

### [response]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `max_chunk_length` | Discord 메시지 최대 문자 수 | `1900` |
| `max_chunks` | 파일 첨부로 전환하기 전 최대 청크 수 | `10` |

### [ratelimit]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `update_interval_secs` | 봇 상태 업데이트 주기 (초) | `60` |
| `alert_thresholds` | 5h 사용률(%) 알림 임계값 | `[50, 80]` |

### [attachment]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `max_file_size_mb` | 파일 1개 최대 크기 (MB) | `25` |
| `max_aggregate_size_mb` | 메시지당 첨부파일 총합 크기 (MB) | `50` |
| `download_timeout_secs` | 파일 다운로드 타임아웃 (초) | `30` |

### language

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `language` | UI 언어: `"ko"` 또는 `"en"` | `"ko"` |

Discord 토큰은 `PIDORY_DISCORD_TOKEN` 환경변수(또는 `.env` 파일)로 설정합니다. `config.toml`에 직접 넣지 마세요.

## Rate Limit 모니터링

pidory는 Claude Code의 API rate limit 사용률을 Discord 봇 상태 메시지로 표시하고 (예: `Watching 5h: 42%(1h30m) | 7d: 38%`), 임계값 초과 시 알림을 보낼 수 있습니다. Rate limit 데이터는 활성 세션의 `stream-json` 출력에서 읽어옵니다.

### 알림 설정

특정 채널로 임계값 알림을 받으려면 `config.toml`의 `[discord]`에 `notification_channel_id`를 설정하세요.

```toml
[discord]
notification_channel_id = "123456789012345678"

[ratelimit]
alert_thresholds = [50, 80]
```

5시간 사용률이 설정된 임계값에 도달하면 알림 채널에 경고가 게시됩니다.

## 라이선스

MIT
