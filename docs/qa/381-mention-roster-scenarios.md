# #381 Mention Roster — QA Scenarios

**Feature**: Roster-based @mention resolution (username / guild_nickname / global_name / alias / Korean-suffix strip)
**Target branch**: `381-mention-roster`
**Base**: develop @ `00b2f4b`
**Executor**: qa-bot (dev environment, isolated `pidory-qa.service`)
**Pass criterion**: each scenario is binary — PASS or FAIL, no partial credit.

---

## Environment Setup (one-time per QA run)

1. Deploy branch `381-mention-roster` to dev environment via `qa-deploy.sh`.
2. Confirm bot is online in the QA guild.
3. All scenarios below must be executed inside a registered **thread** within the QA guild.
4. Before each scenario, ensure the roster DB (`member_roster` table in `pidory_qa` DB) is in the stated precondition. Use direct SQL or gateway events (join/leave) as described.

---

## S-01: Username Exact Substitution

### Precondition
- User **A** is in the QA guild.
- `member_roster` row for guild+userA: `username = "jaemin"`, `guild_nickname = NULL`, `global_name = NULL`, `aliases = []`.
- User **A** is a member of the test thread (scope A: thread join list includes A).

### Steps
1. Send a message in the test thread: `@jaemin 확인해줘`

### Expected Result
- **PASS**: The message delivered to Claude CLI contains `<@{userA_id}> 확인해줘`. No other user is pinged.
- **FAIL**: `@jaemin` appears as plain text, or a different user is pinged.

### Related Code
- `src/handler/mention.rs` — `replace_mentions_with_roster`, stage-1 (`resolve_exact` → `match_field` on `username`)
- `src/mention/roster.rs` — `RosterSnapshot::resolve`, `resolve_exact`

---

## S-02: Guild Nickname Matching

### Precondition
- User **A** is in the QA guild and the test thread.
- `member_roster` row: `username = "jaemin_kr"`, `guild_nickname = "재민"`, `global_name = NULL`, `aliases = []`.
- No other guild member has `guild_nickname = "재민"` (unique).

### Steps
1. Send: `@재민 PR 리뷰 부탁해`

### Expected Result
- **PASS**: Message to Claude CLI contains `<@{userA_id}> PR 리뷰 부탁해`.
- **FAIL**: `@재민` remains plain text, or the wrong user is pinged.

### Related Code
- `src/mention/roster.rs` — `resolve_exact` stage 2 (`match_field` on `guild_nickname`)

---

## S-03: Global Display Name Matching

### Precondition
- User **A** is in the QA guild and the test thread.
- `member_roster` row: `username = "user_a"`, `guild_nickname = NULL`, `global_name = "JaeMin Global"`, `aliases = []`.
- No other guild member has `global_name = "JaeMin Global"` (unique).

### Steps
1. Send: `@JaeMin Global 안녕`

### Expected Result
- **PASS**: Message to Claude CLI contains `<@{userA_id}> 안녕`.
- **FAIL**: `@JaeMin Global` remains plain text, or the wrong user is pinged.

### Related Code
- `src/mention/roster.rs` — `resolve_exact` stage 3 (`match_field` on `global_name`)

---

## S-04: Korean Alias Matching via `/mention alias add`

### Precondition
- User **A** is in the QA guild and the test thread.
- `member_roster` row: `username = "user_a"`, `guild_nickname = NULL`, `global_name = NULL`, `aliases = []` (no alias yet).

### Steps
1. Run slash command: `/mention alias add user:@userA alias:재민`
2. Bot responds with confirmation (ephemeral): `✅ <@userA_id> 에게 \`재민\` 호칭을 등록했어요.`
3. Send a message in the test thread: `@재민 배포 언제야`

### Expected Result (step 2)
- **PASS**: Ephemeral confirmation received; no error message.
- **FAIL**: Error message or no response.

### Expected Result (step 3)
- **PASS**: Message to Claude CLI contains `<@{userA_id}> 배포 언제야`.
- **FAIL**: `@재민` remains plain text.

### Related Code
- `src/commands/mention.rs` — `add` (alias registration + conflict check + DB upsert + cache update)
- `src/mention/roster.rs` — `resolve_exact` stage 4 (`match_alias`)
- `src/db/roster.rs` — `upsert_member`

---

## S-05: Korean Honorific Suffix Strip

### Precondition
- User **A** is in the QA guild and the test thread.
- `member_roster` row: `username = "user_a"`, `guild_nickname = "재민"`, `global_name = NULL`, `aliases = []`.

### Steps
1. Send: `@재민이형 어디야`

### Expected Result
- **PASS**: Message to Claude CLI contains `<@{userA_id}> 어디야`. The suffix `이형` is stripped, `재민` matches guild_nickname, and the substitution is made.
- **FAIL**: `@재민이형` remains plain text (suffix not stripped), or the wrong user is pinged.

### Related Code
- `src/mention/roster.rs` — `resolve` stage 5: `strip_korean_suffix` strips `이형` → `재민`, then `resolve_exact` on stripped name
- Suffix list: `["이형", "이님", "이씨", "형", "님", "씨", "아", "야", "이"]` (longest-match order)

---

## S-06: Ambiguous Name — Two Users With Same Nickname, No Substitution

### Precondition
- User **A** and User **B** are both in the QA guild and the test thread (scope includes both).
- `member_roster`: userA has `guild_nickname = "민수"`, userB has `guild_nickname = "민수"`.

### Steps
1. Send: `@민수 왔어?`

### Expected Result
- **PASS**: Message to Claude CLI contains `@민수 왔어?` unchanged — no user is pinged. Both `<@userA_id>` and `<@userB_id>` are absent. `whitelist` is empty.
- **FAIL**: Either userA or userB is pinged.

### Related Code
- `src/mention/roster.rs` — `unique_or_none`: two candidates → returns `None`
- `src/handler/mention.rs` — `replace_mentions_with_roster`: `roster_longest_match` returns `None` → `@` pushed as plain text

---

## S-07: Heuristic OFF — Vague References Produce No Ping

### Precondition
- `config.toml` (or `config.qa.toml`) has `heuristic_enabled = false` (this is the default; verify no override).
- No roster entry exists for any vague phrase like "그 사람" or "방금".
- At least one other user is in the test thread (non-empty scope).

### Steps
1. Send: `@그 사람한테 물어봐`
2. Send: `@방금 말한 사람`

### Expected Result
- **PASS (step 1)**: Message to Claude CLI contains `@그 사람한테 물어봐` unchanged. No user is pinged.
- **PASS (step 2)**: Message to Claude CLI contains `@방금 말한 사람` unchanged. No user is pinged.
- **FAIL**: Any `<@user_id>` appears in the forwarded message.

### Note
Even if `heuristic_enabled = true`, the heuristic stage is a no-op guard and must never return a UserId (per spec). If testing with `heuristic_enabled = true`, same PASS criterion applies.

### Related Code
- `src/mention/roster.rs` — `resolve` stage 6: `heuristic_enabled` block is intentional no-op, always returns `None`
- `src/mention/roster.rs` — `RosterCache::new(ttl_secs, heuristic_enabled)`

---

## S-08: Hallucination ID Blocking — Scope Whitelist Enforcement

### Precondition
- User **A** (id = `111111111111111111`) is the only member of the test thread (scope A ∪ C = {userA}).
- User **X** (id = `999999999999999999`) exists in the guild but has **never** joined the test thread and has never sent a message there (not in scope C, not in scope A).
- `member_roster` row exists for userX: `username = "ghost_user"`.

### Steps
1. Send a message that references userX by username: `@ghost_user 오류 고쳐줘`

### Expected Result
- **PASS**: Message to Claude CLI contains `@ghost_user 오류 고쳐줘` unchanged. `<@999999999999999999>` does not appear. `whitelist` does not contain userX's id.
- **FAIL**: `<@999999999999999999>` appears — out-of-scope user was pinged.

### Related Code
- `src/mention/roster.rs` — `resolve_exact` → `match_field` filters by `scope.contains(&e.user_id)`. Out-of-scope user fails the filter → `unique_or_none([]) → None`.
- `src/mention/roster.rs` — `RosterSnapshot::resolve` returns `None` for out-of-scope → no whitelist entry.
- `src/handler/mention.rs` — `replace_mentions_with_roster`: only resolved uids enter `whitelist`.

---

## S-09a: Code Block Skip — No Substitution Inside Backtick Fences

### Precondition
- User **A** is in the QA guild and the test thread.
- `member_roster` row: `username = "jaemin"`, `guild_nickname = NULL`, `global_name = NULL`, `aliases = []`.

### Steps
1. Send the following message (triple backtick block):
   ````
   아래 코드에서 @jaemin 변수를 확인해줘:
   ```
   let @jaemin = get_user();
   ```
   ````
2. Send the following message (inline backtick):
   `` `@jaemin` 이 뭐야? ``

### Expected Result (step 1)
- **PASS**: Message to Claude CLI contains `<@{userA_id}>` for the **first** `@jaemin` (outside the fence), and the second `@jaemin` inside the ` ``` ` block remains as literal `@jaemin`.
- **FAIL**: The `@jaemin` inside the code block is substituted to `<@{userA_id}>`.

### Expected Result (step 2)
- **PASS**: Message to Claude CLI contains `` `@jaemin` `` unchanged — no substitution inside inline backtick.
- **FAIL**: The inline `@jaemin` is substituted.

### Related Code
- `src/handler/mention.rs` — `replace_mentions_with_roster`: `in_triple` / `in_inline` state flags skip `@` processing inside backtick fences (reuses `#346` logic)

---

## S-09b: Member Leave — PII Hard Delete and Post-Leave No-Match

### Precondition
- User **A** is currently in the QA guild and the test thread.
- `member_roster` row for userA: `username = "leaver"`, `guild_nickname = "떠난사람"`, `aliases = []`.
- Confirm via `SELECT * FROM member_roster WHERE user_id = {userA_id}` that the row exists.

### Steps
1. User **A** leaves the QA guild (or is kicked).
2. Wait for the `GuildMemberRemoval` gateway event to be processed by the bot (up to 5 seconds).
3. Run SQL: `SELECT COUNT(*) FROM member_roster WHERE guild_id = {qa_guild_id} AND user_id = {userA_id}`.
4. Send a message in the test thread: `@leaver 어디갔어`

### Expected Result (step 3)
- **PASS**: `COUNT(*) = 0` — the row is fully deleted from `member_roster` (hard delete, no soft-delete).
- **FAIL**: `COUNT(*) > 0` — PII row persists after member leave.

### Expected Result (step 4)
- **PASS**: Message to Claude CLI contains `@leaver 어디갔어` unchanged. No user is pinged. `whitelist` is empty.
- **FAIL**: `<@{userA_id}>` appears — deleted member was still resolved.

### Related Code
- `src/handler/message/mod.rs` — `GuildMemberRemoval` event → `delete_roster_member` → `db_roster::delete_member` (SQL `DELETE`) + `roster_cache.remove_entry` (in-memory eviction)
- `src/db/roster.rs` — `delete_member`
- `src/mention/roster.rs` — `RosterCache::remove_entry`

---

## Summary Table

| ID    | Scenario                          | Method       | Pass Criterion                                      |
|-------|-----------------------------------|--------------|-----------------------------------------------------|
| S-01  | Username exact substitution       | Discord msg  | `<@userA_id>` in forwarded message                  |
| S-02  | Guild nickname matching           | Discord msg  | `<@userA_id>` in forwarded message                  |
| S-03  | Global display name matching      | Discord msg  | `<@userA_id>` in forwarded message                  |
| S-04  | Korean alias via `/mention alias` | Slash + msg  | Alias confirmed; `<@userA_id>` in forwarded message |
| S-05  | Korean honorific suffix strip     | Discord msg  | `<@userA_id>` in forwarded message (suffix stripped)|
| S-06  | Ambiguous name — no substitution  | Discord msg  | Plain text `@민수` preserved; whitelist empty        |
| S-07  | Heuristic OFF — no ping           | Discord msg  | Plain text preserved; no `<@user_id>` in message   |
| S-08  | Hallucination ID blocked          | Discord msg  | Out-of-scope user not pinged; whitelist excludes    |
| S-09a | Code block skip                   | Discord msg  | Inside-fence `@name` not substituted                |
| S-09b | Member leave → PII hard delete    | SQL + msg    | DB row deleted; post-leave name not resolved        |
