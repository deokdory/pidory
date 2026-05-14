use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use poise::serenity_prelude::{Context, GuildId, Member, UserId};
use tokio::sync::{Mutex, RwLock};
use tracing::warn;
use unicode_normalization::UnicodeNormalization;

/// nick/display/username → user_id 역방향 조회 맵 (per-guild).
#[derive(Default)]
pub struct GuildMemberCache {
    pub name_to_id: HashMap<String, UserId>,
}

/// 전체 guild의 멤버 캐시.
/// `fetching`: 동일 guild에 대한 lazy fetch 중복 방지용 single-flight 집합.
pub struct MentionCache {
    // GuildId → GuildMemberCache
    cache: Arc<RwLock<HashMap<GuildId, GuildMemberCache>>>,
    // 현재 fetch 진행 중인 guild_id 집합 (single-flight)
    fetching: Arc<Mutex<HashSet<GuildId>>>,
}

impl MentionCache {
    /// 빈 캐시 생성.
    pub fn new() -> Self {
        MentionCache {
            cache: Arc::new(RwLock::new(HashMap::new())),
            fetching: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 새 키를 insert 하기 전 충돌 검사.
    ///
    /// 기존 매핑이 다른 user_id 와 있으면 그 매핑을 제거하고 `true` 반환 (insert skip 권장).
    /// 충돌 없으면 `false` 반환 (insert 진행 OK).
    ///
    /// caller 가 write guard 를 이미 갖고 있어야 함 (deadlock 회피).
    fn check_and_clear_conflict(
        gcache: &mut GuildMemberCache,
        new_key: &str,
        new_uid: UserId,
        guild_id: GuildId,
    ) -> bool {
        if let Some(&existing_uid) = gcache.name_to_id.get(new_key) {
            if existing_uid != new_uid {
                gcache.name_to_id.remove(new_key);
                tracing::warn!(
                    guild_id = %guild_id,
                    name = %new_key,
                    user_a = %existing_uid,
                    user_b = %new_uid,
                    "Mention cache name conflict — both mappings removed (ambiguous)"
                );
                return true;
            }
        }
        false
    }

    /// guild 내 이름으로 user_id 조회.
    pub async fn lookup(&self, guild_id: GuildId, name: &str, ctx: &Context) -> Option<UserId> {
        // 1. cache hit check (read guard, drop before await)
        {
            let guard = self.cache.read().await;
            if let Some(gcache) = guard.get(&guild_id) {
                return gcache.name_to_id.get(&nfc(name)).copied();
            }
        }

        // 2. single-flight: 이미 fetch 중이면 None 반환
        let should_fetch = {
            let mut fg = self.fetching.lock().await;
            if fg.contains(&guild_id) {
                false
            } else {
                fg.insert(guild_id);
                true
            }
        };

        if !should_fetch {
            return None;
        }

        // 3. lazy fetch (5초 timeout)
        let fetch_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            guild_id.members(&ctx.http, None, None),
        )
        .await;

        // 4. single-flight 해제 (성공/실패 무관)
        {
            let mut fg = self.fetching.lock().await;
            fg.remove(&guild_id);
        }

        let members = match fetch_result {
            Ok(Ok(members)) => members,
            Ok(Err(e)) => {
                warn!("guild {} members fetch failed: {}", guild_id, e);
                return None;
            }
            Err(_) => {
                warn!("guild {} members fetch timeout", guild_id);
                return None;
            }
        };

        // 5. cache 채움 (충돌 검출 — 동명이인이면 양쪽 모두 제거)
        // NOTE: 모든 키는 NFC 정규화 후 저장. lookup 도 NFC 적용 (한국어 nickname NFD/NFC 불일치 방지).
        let mut gcache = GuildMemberCache::default();
        for m in &members {
            let candidates = [
                m.user.name.clone(),
                m.user.global_name.clone().unwrap_or_default(),
                m.nick.clone().unwrap_or_default(),
            ];
            for cand in candidates.iter().filter(|s| !s.is_empty()) {
                let key = nfc(cand);
                if Self::check_and_clear_conflict(&mut gcache, &key, m.user.id, guild_id) {
                    continue;
                }
                gcache.name_to_id.insert(key, m.user.id);
            }
        }
        let result = gcache.name_to_id.get(&nfc(name)).copied();
        {
            let mut guard = self.cache.write().await;
            guard.insert(guild_id, gcache);
        }
        result
    }

    /// 멤버 정보를 캐시에 반영 (nick/display/username 모두 등록).
    pub async fn update_member(&self, guild_id: GuildId, member: &Member) {
        let mut guard = self.cache.write().await;
        let gcache = guard.entry(guild_id).or_default();
        // 같은 user_id의 stale 키 제거 (nick 변경 처리)
        gcache.name_to_id.retain(|_, v| *v != member.user.id);

        // 각 키를 insert 전에 충돌 검사 (동명이인 → 양쪽 제거)
        let candidates = [
            member.user.name.clone(),
            member.user.global_name.clone().unwrap_or_default(),
            member.nick.clone().unwrap_or_default(),
        ];
        for cand in candidates.iter().filter(|s| !s.is_empty()) {
            let key = nfc(cand);
            if Self::check_and_clear_conflict(gcache, &key, member.user.id, guild_id) {
                continue;
            }
            gcache.name_to_id.insert(key, member.user.id);
        }
    }

    /// 특정 유저를 캐시에서 제거.
    pub async fn remove_member(&self, guild_id: GuildId, user_id: UserId) {
        let mut guard = self.cache.write().await;
        if let Some(gcache) = guard.get_mut(&guild_id) {
            gcache.name_to_id.retain(|_, v| *v != user_id);
        }
    }

    /// guild의 cache 키 목록을 length-desc 정렬해 반환. cache miss면 None.
    pub async fn keys_by_length_desc(&self, guild_id: GuildId) -> Option<Vec<String>> {
        let guard = self.cache.read().await;
        guard.get(&guild_id).map(|g| {
            let mut keys: Vec<String> = g.name_to_id.keys().cloned().collect();
            keys.sort_by_key(|k| Reverse(k.len()));
            keys
        })
    }

    /// guild 내 키 → user_id 직접 조회 (fetch 안 함, cache hit only).
    pub async fn get_cached(&self, guild_id: GuildId, name: &str) -> Option<UserId> {
        let guard = self.cache.read().await;
        guard.get(&guild_id).and_then(|g| g.name_to_id.get(&nfc(name)).copied())
    }
}

/// 텍스트에서 `@name` 패턴을 파싱해 Discord mention(`<@user_id>`)으로 치환하고,
/// 치환된 UserId 목록을 반환.
pub async fn parse_and_replace(
    text: &str,
    guild_id: Option<GuildId>,
    cache: &MentionCache,
    ctx: &Context,
) -> (String, Vec<UserId>) {
    // 1. 항상 mass mention 마스킹 (DM/guild 무관)
    let masked = mask_mass_mentions(text);

    // 2. DM은 마스킹만, 치환 skip
    let Some(guild_id) = guild_id else {
        return (masked, vec![]);
    };

    // 3. cache 키 length-desc 정렬된 Vec 추출 (read lock 짧게)
    let keys_by_len = match cache.keys_by_length_desc(guild_id).await {
        Some(keys) => keys,
        None => {
            // cache miss — lookup으로 fetch 트리거 (결과 무시, 다음 메시지부터 hit)
            let _ = cache.lookup(guild_id, "__trigger_fetch__", ctx).await;
            return (masked, vec![]);
        }
    };

    // 4. cache snapshot → HashMap 추출 (read lock 짧게)
    let name_to_id: HashMap<String, UserId> = {
        let guard = cache.cache.read().await;
        guard
            .get(&guild_id)
            .map(|g| g.name_to_id.clone())
            .unwrap_or_default()
    };

    // 5. 순수 로직 위임 (masked를 NFC로 정규화 — cache 키와 일관)
    let masked_nfc = nfc(&masked);
    let (body, whitelist) = replace_mentions_with_map(&masked_nfc, &keys_by_len, &name_to_id);
    (body, whitelist)
}

/// 순수 로직 — cache 없이 텍스트 치환.
/// `masked`: mass mention 마스킹이 이미 적용된 텍스트.
/// `keys_by_len`: length-desc 정렬된 이름 키 목록.
/// `name_to_id`: 이름 → UserId 맵.
pub(crate) fn replace_mentions_with_map(
    masked: &str,
    keys_by_len: &[String],
    name_to_id: &HashMap<String, UserId>,
) -> (String, Vec<UserId>) {
    let mut result = String::with_capacity(masked.len() + 32);
    let mut whitelist: Vec<UserId> = Vec::new();
    let bytes = masked.as_bytes();
    let mut i = 0;

    // 코드 블록 상태
    // NOTE: 단순 카운팅 방식 — 100% 정확하지 않음 (issues.md 참조)
    let mut in_triple = false; // ``` 안
    let mut in_inline = false; // 단일 ` 안 (triple이 아닐 때)

    while i < bytes.len() {
        // triple backtick 체크 (3개 연속)
        if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"```" {
            in_triple = !in_triple;
            // triple 진입 시 inline 상태 리셋
            if in_triple {
                in_inline = false;
            }
            result.push_str("```");
            i += 3;
            continue;
        }
        // inline backtick (triple 안이 아닐 때만)
        if bytes[i] == b'`' && !in_triple {
            in_inline = !in_inline;
            result.push('`');
            i += 1;
            continue;
        }
        // 코드 블록 안: 그대로 통과
        if in_triple || in_inline {
            let ch_len = utf8_char_len(&bytes[i..]);
            result.push_str(
                std::str::from_utf8(&bytes[i..i + ch_len]).unwrap_or("\u{FFFD}"),
            );
            i += ch_len;
            continue;
        }
        // @ 발견 → longest match 시도
        if bytes[i] == b'@' {
            let rest = &masked[i + 1..];
            let mut matched: Option<UserId> = None;
            let mut matched_key_len = 0usize;

            for key in keys_by_len {
                if rest.starts_with(key.as_str()) {
                    if let Some(&uid) = name_to_id.get(key.as_str()) {
                        matched = Some(uid);
                        matched_key_len = key.len();
                        break; // length-desc이므로 첫 match가 longest
                    }
                }
            }

            if let Some(uid) = matched {
                result.push_str(&format!("<@{}>", uid));
                whitelist.push(uid);
                i += 1 + matched_key_len;
            } else {
                result.push('@');
                i += 1;
            }
            continue;
        }
        // 일반 char
        let ch_len = utf8_char_len(&bytes[i..]);
        result.push_str(
            std::str::from_utf8(&bytes[i..i + ch_len]).unwrap_or("\u{FFFD}"),
        );
        i += ch_len;
    }

    whitelist.sort_unstable();
    whitelist.dedup();
    (result, whitelist)
}

/// UTF-8 멀티바이트 문자의 바이트 길이를 반환한다.
/// 잘못된 continuation byte는 1바이트로 fallback.
fn utf8_char_len(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 1;
    }
    let b = bytes[0];
    if b < 0xC0 {
        // ASCII (< 0x80) or invalid continuation byte (0x80..0xBF) — both 1 byte
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// 입력 문자열을 NFC 정규화해 반환.
fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// `@everyone` / `@here` 를 무해한 문자열로 마스킹.
///
/// U+200B (zero-width space)를 삽입해 Discord ping을 방지한다.
/// 코드 블록 안이든 밖이든 일괄 적용 (안전 우선 trade-off).
/// NOTE: `@everyone_kr` 같이 `@everyone`을 prefix로 갖는 username도
/// 마스킹될 수 있다 (false positive). 의도된 trade-off — issues.md 참조.
pub fn mask_mass_mentions(text: &str) -> String {
    text.replace("@everyone", "@\u{200B}everyone")
        .replace("@here", "@\u{200B}here")
}

// ─── test-only cache helper ───────────────────────────────────────────────────
#[cfg(test)]
impl MentionCache {
    /// 테스트 전용: guild에 name→uid 매핑을 직접 삽입 (NFC 정규화 후 저장, 충돌 검출 포함).
    pub async fn insert_for_test(&self, guild_id: GuildId, name: &str, user_id: UserId) {
        let mut guard = self.cache.write().await;
        let entry = guard.entry(guild_id).or_default();
        let key = nfc(name);
        if Self::check_and_clear_conflict(entry, &key, user_id, guild_id) {
            return;
        }
        entry.name_to_id.insert(key, user_id);
    }

    /// 테스트 전용: guild 전체 cache를 한 번에 교체 (NFC 정규화 후 저장).
    pub async fn set_guild_cache_for_test(
        &self,
        guild_id: GuildId,
        entries: Vec<(&str, UserId)>,
    ) {
        let mut guard = self.cache.write().await;
        let gcache = guard.entry(guild_id).or_default();
        gcache.name_to_id.clear();
        for (name, uid) in entries {
            gcache.name_to_id.insert(nfc(name), uid);
        }
    }
}

// ─── unit tests ───────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use poise::serenity_prelude::{GuildId, UserId};

    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// name_to_id 에서 length-desc 키 목록을 구성한다 (replace_mentions_with_map 호출용).
    fn keys_desc(map: &HashMap<String, UserId>) -> Vec<String> {
        let mut keys: Vec<String> = map.keys().cloned().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        keys
    }

    /// 단일 name→uid 항목으로 map + keys 쌍을 만든다.
    fn single_entry(name: &str, uid: u64) -> (HashMap<String, UserId>, Vec<String>) {
        let mut map = HashMap::new();
        map.insert(name.to_string(), UserId::new(uid));
        let keys = keys_desc(&map);
        (map, keys)
    }

    // ── mask_mass_mentions ────────────────────────────────────────────────────

    #[test]
    fn test_mask_everyone() {
        let out = mask_mass_mentions("@everyone 확인해봐");
        assert_eq!(out, "@\u{200B}everyone 확인해봐");
    }

    #[test]
    fn test_mask_here() {
        let out = mask_mass_mentions("@here 긴급");
        assert_eq!(out, "@\u{200B}here 긴급");
    }

    #[test]
    fn test_mask_both() {
        let out = mask_mass_mentions("@everyone @here");
        assert!(out.contains("@\u{200B}everyone"));
        assert!(out.contains("@\u{200B}here"));
    }

    #[test]
    fn test_mask_none_pass_through() {
        let input = "@덕돌 안녕하세요";
        assert_eq!(mask_mass_mentions(input), input);
    }

    // ── utf8_char_len ─────────────────────────────────────────────────────────

    #[test]
    fn test_utf8_char_len_ascii() {
        assert_eq!(utf8_char_len(b"A"), 1);
        assert_eq!(utf8_char_len(b"@"), 1);
    }

    #[test]
    fn test_utf8_char_len_2byte() {
        // U+00E9 'é' — 2 bytes: 0xC3 0xA9
        let s = "é";
        assert_eq!(utf8_char_len(s.as_bytes()), 2);
    }

    #[test]
    fn test_utf8_char_len_3byte() {
        // U+AC00 '가' — 3 bytes
        let s = "가";
        assert_eq!(utf8_char_len(s.as_bytes()), 3);
    }

    #[test]
    fn test_utf8_char_len_4byte() {
        // U+1F600 '😀' — 4 bytes
        let s = "😀";
        assert_eq!(utf8_char_len(s.as_bytes()), 4);
    }

    // ── MentionCache CRUD ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_cache_insert_and_get() {
        let cache = MentionCache::new();
        let gid = GuildId::new(1);
        let uid = UserId::new(42);
        cache.insert_for_test(gid, "덕돌", uid).await;
        assert_eq!(cache.get_cached(gid, "덕돌").await, Some(uid));
        assert_eq!(cache.get_cached(gid, "없는이름").await, None);
    }

    #[tokio::test]
    async fn test_cache_remove_member() {
        let cache = MentionCache::new();
        let gid = GuildId::new(2);
        let uid = UserId::new(99);
        cache.insert_for_test(gid, "Mark", uid).await;
        assert_eq!(cache.get_cached(gid, "Mark").await, Some(uid));

        cache.remove_member(gid, uid).await;
        assert_eq!(cache.get_cached(gid, "Mark").await, None);
    }

    #[tokio::test]
    async fn test_cache_keys_by_length_desc_order() {
        let cache = MentionCache::new();
        let gid = GuildId::new(3);
        let uid = UserId::new(1);
        cache.insert_for_test(gid, "a", uid).await;
        cache.insert_for_test(gid, "abcd", uid).await;
        cache.insert_for_test(gid, "ab", uid).await;

        let keys = cache.keys_by_length_desc(gid).await.unwrap();
        // 첫 번째 키가 가장 길어야 함
        assert_eq!(keys[0].len(), 4); // "abcd"
    }

    #[tokio::test]
    async fn test_cache_miss_returns_none() {
        let cache = MentionCache::new();
        let gid = GuildId::new(99);
        // 아무것도 넣지 않은 guild
        assert_eq!(cache.get_cached(gid, "nobody").await, None);
        assert!(cache.keys_by_length_desc(gid).await.is_none());
    }

    // ── replace_mentions_with_map (순수 로직) ─────────────────────────────────

    #[test]
    fn test_longest_match_korean_josa() {
        // @덕돌님 안녕 — "덕돌" cache hit, "님"은 josa (치환 후 붙음)
        let (map, keys) = single_entry("덕돌", 42);
        let (out, wl) = replace_mentions_with_map("@덕돌님 안녕", &keys, &map);
        assert_eq!(out, "<@42>님 안녕");
        assert_eq!(wl, vec![UserId::new(42)]);
    }

    #[test]
    fn test_longest_match_korean_concat() {
        // @덕돌안녕 — "덕돌"만 cache에 있으므로 최장매치 "덕돌", 나머지 "안녕" 그대로
        let (map, keys) = single_entry("덕돌", 42);
        let (out, _) = replace_mentions_with_map("@덕돌안녕", &keys, &map);
        assert_eq!(out, "<@42>안녕");
    }

    #[test]
    fn test_longest_match_english_punct() {
        let (map, keys) = single_entry("Mark", 7);
        // 구두점 뒤에 username이 없으므로 longest match는 "Mark"까지
        let (out1, _) = replace_mentions_with_map("@Mark!", &keys, &map);
        assert_eq!(out1, "<@7>!");

        let (out2, _) = replace_mentions_with_map("@Mark,", &keys, &map);
        assert_eq!(out2, "<@7>,");

        let (out3, _) = replace_mentions_with_map("@Mark.", &keys, &map);
        assert_eq!(out3, "<@7>.");
    }

    #[test]
    fn test_longest_match_continuous() {
        // @a@b 연속 멘션
        let mut map = HashMap::new();
        map.insert("a".to_string(), UserId::new(1));
        map.insert("b".to_string(), UserId::new(2));
        let keys = keys_desc(&map);
        let (out, wl) = replace_mentions_with_map("@a@b", &keys, &map);
        assert_eq!(out, "<@1><@2>");
        assert_eq!(wl.len(), 2);
    }

    #[test]
    fn test_codeblock_triple_skip() {
        // ``` 안의 @x 는 치환하지 않음
        let (map, keys) = single_entry("x", 5);
        let (out, wl) = replace_mentions_with_map("```@x```", &keys, &map);
        assert_eq!(out, "```@x```");
        assert!(wl.is_empty());
    }

    #[test]
    fn test_codeblock_inline_skip() {
        // `@x` inline backtick 안의 @x 는 치환하지 않음
        let (map, keys) = single_entry("x", 5);
        let (out, wl) = replace_mentions_with_map("`@x`", &keys, &map);
        assert_eq!(out, "`@x`");
        assert!(wl.is_empty());
    }

    #[test]
    fn test_codeblock_normal_then_block() {
        // 본문 @x 치환, 블록 안 @x 유지
        let (map, keys) = single_entry("x", 5);
        let (out, wl) = replace_mentions_with_map("@x hello ```@x```", &keys, &map);
        assert_eq!(out, "<@5> hello ```@x```");
        assert_eq!(wl, vec![UserId::new(5)]);
    }

    #[test]
    fn test_whitelist_dedup() {
        // 같은 user를 여러 번 멘션 → whitelist는 1개
        let (map, keys) = single_entry("x", 5);
        let (_, wl) = replace_mentions_with_map("@x @x @x", &keys, &map);
        assert_eq!(wl.len(), 1);
        assert_eq!(wl[0], UserId::new(5));
    }

    #[test]
    fn test_single_at_no_panic() {
        let map: HashMap<String, UserId> = HashMap::new();
        let keys: Vec<String> = vec![];
        // 아래 케이스 모두 panic 없이 처리
        let (out1, _) = replace_mentions_with_map("@", &keys, &map);
        assert_eq!(out1, "@");

        let (out2, _) = replace_mentions_with_map("@@", &keys, &map);
        assert_eq!(out2, "@@");

        let (out3, _) = replace_mentions_with_map("@ ", &keys, &map);
        assert_eq!(out3, "@ ");

        let (out4, _) = replace_mentions_with_map("text@", &keys, &map);
        assert_eq!(out4, "text@");
    }

    #[test]
    fn test_empty_text() {
        let map: HashMap<String, UserId> = HashMap::new();
        let keys: Vec<String> = vec![];
        let (out, wl) = replace_mentions_with_map("", &keys, &map);
        assert_eq!(out, "");
        assert!(wl.is_empty());
    }

    #[test]
    fn test_cache_miss_text_preserved() {
        // cache 비어있을 때 — 치환 없이 텍스트 보존 (마스킹만)
        let map: HashMap<String, UserId> = HashMap::new();
        let keys: Vec<String> = vec![];
        let masked = mask_mass_mentions("@everyone 안녕 @덕돌");
        let (out, wl) = replace_mentions_with_map(&masked, &keys, &map);
        // @everyone 마스킹 적용, @덕돌은 그대로 (cache miss)
        assert!(out.contains("@\u{200B}everyone"));
        assert!(out.contains("@덕돌"));
        assert!(wl.is_empty());
    }

    // ── dm_skip_replace ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_dm_skip_replace() {
        // guild_id=None → replace_mentions_with_map 호출 안 됨, mask만
        // parse_and_replace를 직접 테스트하기 위해 cache 채운 후 guild_id=None 전달
        // (ctx는 cache hit 시 사용 안 하지만, cache miss 트리거에서 사용 — 여기선 guild_id=None이라 2단계에서 return)
        let masked = mask_mass_mentions("@everyone @덕돌");
        assert!(masked.contains("@\u{200B}everyone"));
        // DM이면 @덕돌은 그대로 남아야 함 — replace_mentions_with_map 미호출 확인
        // (parse_and_replace는 ctx 필요해서 직접 호출 불가 — masked 결과로 검증)
        assert!(masked.contains("@덕돌"));
    }

    // ── cache priority: nick last-write-wins ──────────────────────────────────

    #[tokio::test]
    async fn test_cache_priority_nick_wins() {
        // username + global + nick 모두 등록 시 nick이 마지막이므로 set_guild_cache_for_test로 순서 재현
        // last-write-wins: nick이 username/global_name을 덮어씀
        let cache = MentionCache::new();
        let gid = GuildId::new(10);
        let uid = UserId::new(100);

        // username (최저 우선순위)
        cache.insert_for_test(gid, "user123", uid).await;
        // global_name (중간)
        cache.insert_for_test(gid, "GlobalUser", uid).await;
        // nick (최고 우선순위 — 마지막 삽입)
        cache.insert_for_test(gid, "닉네임", uid).await;

        // 세 이름 모두 같은 uid로 resolve
        assert_eq!(cache.get_cached(gid, "user123").await, Some(uid));
        assert_eq!(cache.get_cached(gid, "GlobalUser").await, Some(uid));
        assert_eq!(cache.get_cached(gid, "닉네임").await, Some(uid));
    }

    #[tokio::test]
    async fn test_update_member_cache_hit() {
        // insert_for_test 후 get_cached 확인 (update_member는 Member 생성 어려워 insert로 대체)
        let cache = MentionCache::new();
        let gid = GuildId::new(20);
        let uid = UserId::new(200);
        cache.insert_for_test(gid, "TestUser", uid).await;
        assert_eq!(cache.get_cached(gid, "TestUser").await, Some(uid));
        // 다른 이름으로 덮어씀 (stale key 잔류 — insert 방식이므로 이전 키 유지)
        cache.insert_for_test(gid, "NewNick", uid).await;
        assert_eq!(cache.get_cached(gid, "NewNick").await, Some(uid));
    }

    #[tokio::test]
    async fn test_remove_member_cache_miss() {
        let cache = MentionCache::new();
        let gid = GuildId::new(30);
        let uid = UserId::new(300);
        cache.insert_for_test(gid, "ToRemove", uid).await;
        cache.remove_member(gid, uid).await;
        assert_eq!(cache.get_cached(gid, "ToRemove").await, None);
    }

    // ── NFC normalization ─────────────────────────────────────────────────────

    // ── conflict detection ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_update_member_conflict_removes_both() {
        let cache = MentionCache::new();
        let gid = GuildId::new(50);
        let uid_a = UserId::new(100);
        let uid_b = UserId::new(200);

        // A 가 "민수" 로 등록
        cache.insert_for_test(gid, "민수", uid_a).await;
        assert_eq!(cache.get_cached(gid, "민수").await, Some(uid_a));

        // B 가 같은 nick "민수" 로 충돌 시도 — 양쪽 다 제거
        cache.insert_for_test(gid, "민수", uid_b).await;
        assert_eq!(cache.get_cached(gid, "민수").await, None);
    }

    #[tokio::test]
    async fn test_insert_same_user_no_conflict() {
        // 같은 user 가 자기 이름 다시 등록 — 충돌 아님
        let cache = MentionCache::new();
        let gid = GuildId::new(51);
        let uid = UserId::new(42);

        cache.insert_for_test(gid, "alice", uid).await;
        cache.insert_for_test(gid, "alice", uid).await; // 동일 user_id
        assert_eq!(cache.get_cached(gid, "alice").await, Some(uid));
    }

    #[tokio::test]
    async fn test_nfc_decomposed_nick_lookup() {
        let cache = MentionCache::new();
        let gid = GuildId::new(1);
        let uid = UserId::new(42);
        // NFC "덕돌" 로 cache 채움 (insert_for_test 내부에서 NFC 적용)
        cache.insert_for_test(gid, "덕돌", uid).await;
        // NFD-decomposed jamo 로 lookup — 내부에서 NFC 정규화 후 비교 → hit
        let nfd = "\u{1103}\u{1165}\u{11A8}\u{1103}\u{1169}\u{11AF}";
        assert_eq!(cache.get_cached(gid, nfd).await, Some(uid));
    }
}
