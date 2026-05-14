# pidory

<p align="center">
  <img src="assets/pidory.png" width="256" alt="pidory">
</p>

[![라이선스](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://blog.rust-lang.org/2024/10/17/Rust-2024-edition.html)
[![버전](https://img.shields.io/badge/version-v0.7.0-green.svg)](https://github.com/deokdory/pidory/releases)

[English](./README.md) | **한국어**

## 개요

pidory는 Discord 스레드를 [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI 세션에 연결하는 Discord 봇이에요. `stream-json` IPC를 통해 각 Discord 스레드가 장기 실행 Claude Code 서브프로세스에 매핑돼요. 도구 권한 요청은 Discord 버튼으로 표시되고, 파일 첨부는 양방향으로 처리되며, 멀티유저 sender prefix로 컨텍스트를 명확하게 유지해요. Rate limit 사용률은 봇 상태 메시지로 확인할 수 있어요.

**핵심 가치**

- **스레드별 세션** — Discord 스레드마다 독립적인 Claude Code 서브프로세스, 세션 간 오염 없음
- **권한 버튼** — Allow / Always Allow / Deny를 Discord 버튼으로 처리
- **양방향 파일 첨부** — Discord에서 Claude Code로 파일 업로드, Claude Code에서 Discord로 파일 수신
- **멀티유저 인식** — 멀티유저 스레드에서 메시지마다 발신자 이름 prefix를 주입해 Claude가 누가 말하는지 파악

## 기능

- **스레드 기반 세션** — 각 스레드가 독립적인 Claude Code 서브프로세스에 매핑
- **도구 권한 관리** — Discord 버튼으로 Allow / Always Allow / Deny 처리
- **파일 첨부** — Discord에서 파일 업로드 → Claude Code 전달, Claude Code 생성 파일 → Discord 수신
- **멀티유저 sender prefix** — 멀티유저 스레드에서 발신자 표시 이름을 메시지 앞에 자동 추가
- **`/update` 사전 검증** — 최신 릴리스를 가져와 소스에서 빌드 후 서비스를 재시작
- **i18n** — 한국어(기본) / 영어 UI 지원; `config.toml`에서 `language = "ko"` / `"en"` 설정
- **Rate limit 모니터링** — 봇 상태 메시지로 5h/7d 사용률(%) 표시, 임계값 초과 시 알림
- **`/branch` 세션 포크** — 현재 세션을 새 스레드로 복제 (선택적 컨텍스트 스냅샷 포함)
- **`/sleep` 세션 일시 중단** — 서브프로세스를 해제하면서 스레드 상태는 유지
- **`/skill` 호출** — Discord에서 Claude Code 스킬(슬래시 커맨드) 직접 실행
- **첨부파일 다운로드** — Discord 메시지에 첨부된 파일을 프로젝트 디렉토리에 자동 다운로드
- **PostgreSQL 백엔드** — 프로덕션급 영속성; 레거시 SQLite에서 마이그레이션 경로 제공

## 사전 준비

- **Rust 1.85+** (2024 edition)
- **PostgreSQL 14+** (17 권장) — Linux에서는 `scripts/postgres-setup.sh`로 자동 설치
- **[Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code)** — Anthropic Max 구독 필요
- **Discord Bot Token** — `MESSAGE CONTENT INTENT` 활성화 필요
- **Linux** (systemd) 또는 **macOS** (launchd)

## 설치

단계별 설정 가이드(Discord 봇 생성, PostgreSQL 설정, 서비스 배포)는 [INSTALL.md](./INSTALL.md)를 참고해 주세요.

**빠른 시작 (Linux):**

```bash
git clone https://github.com/deokdory/pidory.git && cd pidory
echo 'PIDORY_DISCORD_TOKEN=your_token_here' > .env
bash deploy/install.sh                # 빌드 + 서비스 + pidory-migrate 설치
sudo bash scripts/postgres-setup.sh   # PostgreSQL 설정 (install.sh 완료 후 실행)
$EDITOR config.toml                   # guild_id, owner_id 입력
sudo systemctl start pidory
```

## 설정

`config.toml`이 모든 런타임 동작을 제어해요. `config.toml.example`을 `config.toml`로 복사한 뒤 필수 필드를 입력해 주세요.

### 주요 섹션

#### [discord]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `guild_id` | Discord 서버 ID | **필수** |
| `owner_id` | 봇 소유자 Discord 사용자 ID | **필수** |
| `token_env` | Discord 토큰 환경변수 이름 | `"PIDORY_DISCORD_TOKEN"` |
| `notification_channel_id` | Rate limit 알림을 보낼 채널 ID | — |
| `project_roots` | `/register`에서 경로 자동완성에 사용할 루트 디렉토리 | `[]` |
| `default_category_id` | `/new-project`의 기본 카테고리 ID | — |

#### [claude]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `binary_path` | `claude` CLI 바이너리 경로 | `"claude"` |
| `default_disallowed_tools` | 새 세션의 기본 차단 도구 목록 | `[]` |
| `subprocess_timeout_secs` | Claude Code 서브프로세스 최대 실행 시간 (초) | `600` |
| `max_sessions` | 최대 동시 세션 수 | `10` |
| `idle_timeout_secs` | 유휴 세션 타임아웃 (초, 0이면 비활성화) | `7200` |

#### [response]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `max_chunk_length` | Discord 메시지 최대 문자 수 | `1900` |
| `max_chunks` | 파일 첨부로 전환하기 전 최대 청크 수 | `10` |

#### [attachment]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `max_file_size_mb` | 파일 1개 최대 크기 (MB) | `25` |
| `max_aggregate_size_mb` | 메시지당 첨부파일 총합 크기 (MB) | `50` |
| `download_timeout_secs` | 파일 다운로드 타임아웃 (초) | `30` |

#### [ratelimit]

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `update_interval_secs` | 봇 상태 업데이트 주기 (초) | `60` |
| `alert_thresholds` | 5h 사용률(%) 알림 임계값 | `[50, 80]` |

### 환경변수

| 변수 | 설명 |
|------|------|
| `PIDORY_DISCORD_TOKEN` | Discord 봇 토큰 — **config.toml에 절대 넣지 마세요** |
| `DATABASE_URL` | PostgreSQL 연결 문자열 (권위 있는 소스) |
| `PIDORY_CONFIG` | config.toml 경로 (기본값: `./config.toml`) |
| `PIDORY_LOCALE` | UI 로케일 강제 지정 (`ko` 또는 `en`) |
| `RUST_LOG` | 로그 필터 (예: `pidory=debug,warn`) |

> **참고:** `config.toml`의 `[database] path` 필드는 **deprecated**이며 런타임에 무시돼요. `DATABASE_URL`만 사용해 주세요.

## 사용법

### 슬래시 커맨드

| 커맨드 | 설명 | 권한 |
|--------|------|------|
| `/register <path> [name]` | 현재 채널에 프로젝트 디렉토리 등록 | MANAGE_CHANNELS |
| `/unregister` | 현재 채널의 프로젝트 등록 해제 | MANAGE_CHANNELS |
| `/new-project <path> [name]` | 새 프로젝트용 채널 + 스레드 생성 | 소유자 전용 |
| `/list [channel]` | 채널의 활성 세션 목록 조회 | MANAGE_CHANNELS |
| `/status [thread_id]` | 세션 상태 확인 | MANAGE_CHANNELS |
| `/sessions` | 전역 세션 현황 조회 (세션 수, 유휴 시간, 상태) | MANAGE_CHANNELS |
| `/del [thread_id]` | 세션 삭제 (기본값: 현재 스레드) | MANAGE_CHANNELS |
| `/stop` | 현재 턴의 Claude Code 프로세스 중단 | turn 시작자 또는 소유자 |
| `/skill <name> [args]` | Claude Code 세션에 스킬(예: `/commit`) 실행 | 모든 멤버 |
| `/recall` | Claude Code에 전달되기 전 대기 중인 메시지 회수 | turn 시작자 |
| `/branch [context]` | 현재 세션을 새 스레드로 포크 (선택적 컨텍스트 포함) | 소유자 전용 |
| `/model <model_name>` | 현재 세션의 Claude 모델 전환 | 모든 멤버 |
| `/sleep` | 세션 일시 중단 (서브프로세스 해제, 스레드 상태 유지) | 모든 멤버 |
| `/update` | 최신 릴리스를 가져와 소스 빌드 후 서비스 재시작 | 소유자 전용 |

### Claude Code와 대화하기

1. 채널에서 `/register /path/to/project` 실행
2. 해당 채널에서 스레드를 열면 — 각 스레드가 독립적인 Claude Code 세션
3. 스레드에서 메시지를 보내면 Claude Code가 응답
4. 도구 권한 요청 버튼이 나타나면 클릭해서 응답:
   - **Allow** — 이 도구 호출 한 번만 허용
   - **Always Allow** — 이 세션에서 이 도구를 항상 자동 허용
   - **Deny** — 거부

### 세션 리셋

스레드에서 일반 메시지로 `/clear` 또는 `/new`를 입력하면 세션을 리셋할 수 있어요. 확인 버튼이 표시되며 **확인**을 누르면 재시작, **취소**를 누르면 현재 세션이 유지돼요.

### 파일 첨부

Discord 메시지에 파일을 첨부하면 pidory가 프로젝트 디렉토리에 다운로드하고 파일 경로를 Claude Code에 전달해요. Claude Code가 출력 파일을 생성하면 Discord 파일 첨부로 수신돼요.

### 답장 컨텍스트

스레드 내 메시지에 답장(Reply)하면 pidory가 원본 메시지 내용을 추출해 프롬프트에 컨텍스트로 주입해요.

## 권한 모델

pidory는 Claude Code의 권한 요청을 Discord 버튼으로 표시해요: **Allow**, **Always Allow**, **Deny**.

- **Allow** — 이 요청에 한해 도구 호출 허용
- **Always Allow** — 도구를 세션의 항상-허용 목록에 추가; 이후 같은 도구 요청은 자동 승인
- **Deny** — 도구 호출 거부

권한 버튼은 해당 턴을 시작한 사용자(또는 `owner_id`)만 클릭할 수 있어요.

### ⚠️ 멀티유저 베타 — Always Allow는 스레드 내 모든 사용자에게 적용돼요

세션은 스레드 단위로 공유돼요. 한 사용자가 **Always Allow**를 클릭하면 그 권한은 **세션 전체**에 적용돼요 — 이후 같은 스레드의 어떤 사용자가 보내는 메시지에도 해당 도구가 자동 승인돼요. pidory는 서로 신뢰하는 사용자들로만 구성된 서버에서 사용해 주세요. 악의적인 사용자가 Always Allow를 악용하면 임의 코드 실행, 파일 조작, 다른 사용자 권한 승격이 가능해요.

## 업그레이드

### `/update` 커맨드 사용 (권장)

`/update` 슬래시 커맨드(소유자 전용)로 안내에 따라 인플레이스 업데이트를 수행해요:

1. `DATABASE_URL` 설정 여부 및 접근 가능 여부 확인
2. 현재 데이터베이스의 `pg_dump` 백업 자동 생성
3. `git fetch` 후 최신 릴리스 태그로 워킹트리 리셋
4. `cargo build --release`로 바이너리 재빌드
5. 응답 메시지 전송 후 약 30초 지연 서비스 재시작

사전 검사 및 롤백 동작의 전체 내용은 `INSTALL.md` → "Updating"을 참고해 주세요.

### 수동 업데이트

수동 명령 순서는 [INSTALL.md → Updating](./INSTALL.md#updating)을 참고해 주세요.

### SQLite → PostgreSQL 마이그레이션 (v0.7.0 Breaking Change)

v0.7.0부터 SQLite 지원이 완전히 제거돼요. v0.6.x에서 업그레이드하는 경우 [`docs/release-notes/v0.7.0.md`](./docs/release-notes/v0.7.0.md)의 마이그레이션 가이드를 따라 주세요.

`pidory-migrate` 바이너리(`deploy/install.sh`가 `/usr/local/bin/`에 설치)가 일회성 데이터 임포트를 처리해요:

```bash
# PostgreSQL DB가 비어 있으면 서비스 시작 시 ExecStartPre로 자동 실행
# 수동 실행:
pidory-migrate
```

마이그레이션은 roll-forward only — SQLite로 되돌리는 경로는 없어요.

## 아키텍처

### 데이터 흐름

```
Discord 메시지
  → handler::message
    → SessionManager::send_message (mpsc 큐, 스레드별 워커)
      → Claude CLI 서브프로세스 (stream-json stdin/stdout)
        → parser::parse_line → StreamEvent
  → handler::message ← event_rx (mpsc 채널)
→ Discord 응답
```

### 모듈 구조

| 모듈 | 역할 |
|------|------|
| `subprocess/session_manager.rs` | Claude CLI 스폰; 스레드당 하나의 워커 태스크. stdin 쓰기, stdout 파싱, mid-turn 주입, 권한 흐름 처리 |
| `subprocess/parser/` | JSON 라인 → `StreamEvent` 파싱 (Init, Assistant, User, RateLimit, Result, ControlRequest 등); `raw.rs`, `events.rs` 등으로 분리 |
| `subprocess/permission.rs` | `PermissionCache` (세션별 Always Allow 집합); oneshot 채널을 통한 `PermissionRequest`/`PermissionDecision` |
| `handler/message/` | Discord 이벤트를 세션 큐로 라우팅; 500ms fast-complete 판정; Discord에 실시간 스트리밍; `mod.rs`, `event_processor.rs`, `interaction.rs` 등으로 분리 |
| `handler/formatter.rs` | 2000자 제한 코드 블록 인식 메시지 분할 |
| `handler/permission_ui.rs` | Allow/Always Allow/Deny 버튼 메시지 생성; `perm:{id}:{action}` 커스텀 ID 파싱 |
| `handler/status.rs` | `StatusMessage` — 도구 이력을 표시하는 단일 편집 가능 Discord 메시지 (1.5초 rate limit) |
| `commands/` | 슬래시 커맨드 — poise로 선언, 길드 스코프 |
| `db/` | sqlx를 통한 PostgreSQL; 컴파일 타임 검증 마이그레이션; status 컬럼 CAS를 통한 세션 잠금 |
| `i18n/` | 한국어 / 영어 메시지 카탈로그; 런타임 로케일 선택 |

## 이슈

버그 리포트와 기능 요청은 [GitHub Issues](https://github.com/deokdory/pidory/issues)에서 환영해요. 현재의 기여 정책은 [CONTRIBUTING.md](./CONTRIBUTING.md)를 참고해 주세요.

## 라이선스

pidory는 Apache License, Version 2.0으로 라이선스가 부여돼요. 전문은 [LICENSE](./LICENSE)를 참고해 주세요.

## 감사

- [Anthropic](https://www.anthropic.com) — Claude Code CLI와 stream-json 프로토콜
- [poise](https://github.com/serenity-rs/poise) / [serenity](https://github.com/serenity-rs/serenity) — Rust용 Discord 프레임워크
- [sqlx](https://github.com/launchbadge/sqlx) — 컴파일 타임 쿼리 검증을 지원하는 비동기 PostgreSQL 드라이버
- [tokio](https://tokio.rs) — 동시 세션 워커 모델을 구동하는 비동기 런타임
