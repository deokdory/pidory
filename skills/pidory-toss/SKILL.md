---
name: pidory-toss
description: "Discord(pidory bot) 세션에서 파일을 첨부로 전송한다. /pidory-toss <파일경로> [파일경로2 ...]"
argument-hint: "<파일경로> [파일경로2 ...]"
---

# Pidory Toss

pidory Discord bot 세션 전용 스킬. Claude가 마커를 텍스트에 출력하면 pidory가 이를 감지하여 Discord 파일 첨부로 변환한다.

**주의**: 이 스킬은 pidory bot을 통한 Discord 세션에서만 동작한다. 일반 Claude CLI 세션에서는 마커만 출력되고 실제 첨부는 되지 않는다.

## 사용법

```
/pidory-toss <파일경로>
/pidory-toss <파일경로1> <파일경로2> ...
```

## 절차

### 1. 인수 파싱

`$ARGUMENTS`를 공백으로 분리하여 파일 경로 목록을 얻는다.

파일 경로가 하나도 없으면: "어떤 파일을 보낼까?" 질문 후 종료.

### 2. 각 파일에 대해 검증 (Bash 사용)

```bash
# 예시: /tmp/report.pdf 검증
test -f "/tmp/report.pdf" || echo "NOT_FOUND"
realpath "/tmp/report.pdf"
wc -c < "/tmp/report.pdf"
```

아래 순서로 각 파일을 처리한다:

1. **존재 확인**: `test -f <경로>` — 실패 시 → "파일을 찾을 수 없음: {경로}" 에러 메시지 출력, 해당 파일 건너뜀
2. **절대 경로 변환**: `realpath <경로>` — 상대 경로를 절대 경로로 변환
3. **크기 확인**: `wc -c < <절대경로>` — 26214400 bytes(25MB) 초과 시 → "파일이 너무 큼 (25MB 초과): {경로}" 에러 메시지 출력, 해당 파일 건너뜀

### 3. 마커 출력 (검증 통과한 파일만)

검증을 통과한 각 파일에 대해 **Claude 텍스트 응답에** 아래 마커를 포함한다:

```
<!--pidory:attach:{절대경로}-->
```

예시: 파일이 `/home/user/report.pdf`라면:

```
<!--pidory:attach:/home/user/report.pdf-->
```

**중요**: 마커는 Bash stdout이 아니라 Claude의 텍스트 응답(Assistant message)에 직접 포함되어야 한다. pidory bot이 `ContentBlock::Text`에서 이 패턴을 감지한다.

### 4. 사용자 메시지 출력

모든 처리가 끝나면 아래와 같이 요약한다:

- 성공한 파일: "파일 전송 요청됨: {파일명}" (각각)
- 실패한 파일: 에러 이유와 함께 안내

## 출력 예시

파일 `/tmp/data.csv`를 전송하는 경우:

```
<!--pidory:attach:/tmp/data.csv-->

파일 전송 요청됨: data.csv
```

파일 2개 중 1개가 없는 경우:

```
파일을 찾을 수 없음: ./missing.txt

<!--pidory:attach:/home/user/logs/app.log-->

파일 전송 요청됨: app.log
```

## 규칙

- 마커는 반드시 Claude 텍스트 응답에 포함 (Bash echo로 출력하면 안 됨)
- 절대 경로만 마커에 사용 (항상 `realpath`로 변환)
- 25MB(26,214,400 bytes) 초과 파일은 첨부 불가 — Discord 한도
- 이 스킬은 pidory bot Discord 세션 전용
