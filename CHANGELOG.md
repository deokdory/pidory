# CHANGELOG

## Unreleased

### Added — `/update` 슬래시 커맨드 (#237)

owner 전용 자가 업데이트 커맨드. Discord에서 한 번의 명령으로 봇을 최신 GitHub release 태그로 업데이트한다.

**사용법:**
```
/update              # 최신 release 확인 후 업데이트
/update force:true   # 활성 턴 무시하고 강제 재빌드/재시작
```

**동작:**
1. 최신 릴리스 태그 조회
2. 현재 버전과 비교 (다운그레이드 자동 거부)
3. 활성 턴 체크 (force=false 시)
4. `git fetch --tags` + `git reset --hard refs/tags/<latest>`
5. 바이너리 + SQLite DB 백업 (WAL-safe, 1개 보관)
6. `cargo build --release`
7. `~/.claude/skills/` 동기화 (atomic rename)
8. 업데이트 마커 생성 → `pidory-delayed-restart.service` 스케줄 (30초 후 재시작)

**자동 롤백:**
부팅 실패 시 봇이 자체 판단으로 백업 바이너리/DB로 복구 후 재시작. SSH/PC 접근 불가 상황 (모바일 Discord)에서도 안전.
- 1회 롤백 시도 → 성공하면 정상 운영
- 2회 연속 실패 → 무한 루프 방지를 위해 중단

**전제 조건:**
- `sqlite3` CLI가 `$PATH`에 있어야 함 (DB 백업)
- `deploy/install.sh` 실행으로 `pidory-delayed-restart.service` 설치 필요
- polkit rule `50-pidory.rules` (user 권한으로 systemctl 호출 허용)
- Linux only (macOS는 best-effort, `launchctl kickstart`)

**권한:** owner (config `discord.owner_id`) 전용. 다른 사용자는 ephemeral "권한 없음" 응답.

### Other changes

- `deploy/pidory-delayed-restart.service` repo에 포함 (`install.sh`가 자동 설치)
- `Cargo.toml` 버전 `0.6.3` → `0.6.4` 동기화 (git tag와 일치)
- `deploy/RELEASE.md` 신규 — release 워크플로 문서
