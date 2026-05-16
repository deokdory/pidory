//! Roster cache — guild member SoT (Source of Truth) for @name resolution.
//!
//! # Design
//!
//! - **RosterCache** holds an in-memory per-guild map of `UserId → RosterEntry`.
//! - Data is loaded from `db::roster` (PostgreSQL). Stale entries (TTL expired)
//!   are refreshed from DB on next access.
//! - **Channel scope** (A+C hybrid): A = thread join list via Discord API,
//!   C = recorded speakers. `channel_scope()` returns A ∪ C.
//! - **Resolution** is multi-stage: username exact → guild_nickname → global_name
//!   → alias → Korean suffix strip (if `korean_match_mode == "suffix_strip"`) → [heuristic guard] → None.
//! - `heuristic_enabled=false` (default) means the heuristic stage returns None.
//!   Even when enabled, it **never** returns a UserId — it always returns None.
//!   This is the primary guardrail against accidental pings.
//! - Duplicate name matches (ambiguous) → None. Scope-filtered (not in scope) → None.
//! - **RosterSnapshot** is a point-in-time copy for use during a single turn
//!   (protects against member-leave races mid-turn).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, GuildId, Http, UserId};
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::warn;
use unicode_normalization::UnicodeNormalization;

use crate::db::roster as db_roster;

// ─── Korean suffix strip constants ───────────────────────────────────────────

/// 한글 호칭 접미 목록 — **긴 것 먼저** (longest-match 정신).
/// strip 우선순위: 이형 > 형 > 이님 > 님 > 이씨 > 씨 > 아 > 야 > 이
const KOREAN_SUFFIXES: &[&str] = &[
    "이형", "이님", "이씨", "형", "님", "씨", "아", "야", "이",
];

// ─── Data structures ──────────────────────────────────────────────────────────

/// Per-user entry in the roster cache.
#[derive(Debug, Clone)]
pub struct RosterEntry {
    pub user_id: UserId,
    pub username: String,
    pub global_name: Option<String>,
    pub guild_nickname: Option<String>,
    pub aliases: Vec<String>,
}

/// Point-in-time immutable copy of a guild's roster.
///
/// Callers should call `RosterCache::snapshot()` at turn start and pass it to
/// `RosterSnapshot::resolve()` to avoid member-leave races mid-turn.
#[derive(Clone, Debug, Default)]
pub struct RosterSnapshot {
    /// user_id → entry map (NFC keys already stored in `RosterEntry` fields).
    entries: HashMap<UserId, RosterEntry>,
    heuristic_enabled: bool,
    /// korean_match_mode from config. "suffix_strip" enables stage 5; any other value skips it.
    korean_match_mode: String,
}

impl RosterSnapshot {
    /// Resolve a raw name string to a UserId, constrained to `scope`.
    ///
    /// Resolution order:
    /// 1. username exact (NFC)
    /// 2. guild_nickname exact (NFC)
    /// 3. global_name exact (NFC)
    /// 4. alias exact (NFC, any alias in the list)
    /// 5. Korean suffix strip then re-try stages 1-4 (only when `korean_match_mode == "suffix_strip"`)
    /// 6. heuristic guard → always None
    ///
    /// Duplicate match (≥2 users share the same key) → None.
    /// User not in scope → excluded from matching.
    pub fn resolve(&self, raw_name: &str, scope: &HashSet<UserId>) -> Option<UserId> {
        let name_nfc = nfc(raw_name);

        // Stage 1-4: exact match on NFC-normalized fields
        if let Some(uid) = self.resolve_exact(&name_nfc, scope) {
            return Some(uid);
        }

        // Stage 5: Korean suffix strip → re-try exact (only when mode is "suffix_strip")
        if self.korean_match_mode == "suffix_strip"
            && let Some(stripped) = strip_korean_suffix(&name_nfc)
            && let Some(uid) = self.resolve_exact(&stripped, scope)
        {
            return Some(uid);
        }

        // Stage 6: heuristic guard — NEVER returns UserId
        if self.heuristic_enabled {
            // Intentional no-op: heuristic may be expanded in the future,
            // but per spec it must never produce a UserId (plain-text-only guardrail).
            return None;
        }

        None
    }

    /// Internal: exact multi-stage match, scope-filtered and ambiguity-checked.
    fn resolve_exact(&self, name_nfc: &str, scope: &HashSet<UserId>) -> Option<UserId> {
        // Stage 1: username
        let candidates = self.match_field(name_nfc, scope, |e| Some(e.username.as_str()));
        if let Some(uid) = unique_or_none(candidates) {
            return Some(uid);
        }

        // Stage 2: guild_nickname
        let candidates =
            self.match_field(name_nfc, scope, |e| e.guild_nickname.as_deref());
        if let Some(uid) = unique_or_none(candidates) {
            return Some(uid);
        }

        // Stage 3: global_name
        let candidates = self.match_field(name_nfc, scope, |e| e.global_name.as_deref());
        if let Some(uid) = unique_or_none(candidates) {
            return Some(uid);
        }

        // Stage 4: alias (any alias in the vec matches)
        let candidates = self.match_alias(name_nfc, scope);
        if let Some(uid) = unique_or_none(candidates) {
            return Some(uid);
        }

        None
    }

    /// Collect user_ids where `field_fn(entry)` == name_nfc, filtered by scope.
    fn match_field<'a, F>(&'a self, name_nfc: &str, scope: &HashSet<UserId>, field_fn: F) -> Vec<UserId>
    where
        F: Fn(&'a RosterEntry) -> Option<&'a str>,
    {
        self.entries
            .values()
            .filter(|e| scope.contains(&e.user_id))
            .filter_map(|e| {
                let field = field_fn(e)?;
                if nfc(field) == name_nfc {
                    Some(e.user_id)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Collect user_ids where any alias == name_nfc, filtered by scope.
    fn match_alias(&self, name_nfc: &str, scope: &HashSet<UserId>) -> Vec<UserId> {
        self.entries
            .values()
            .filter(|e| scope.contains(&e.user_id))
            .filter(|e| e.aliases.iter().any(|a| nfc(a.as_str()) == name_nfc))
            .map(|e| e.user_id)
            .collect()
    }
}

/// Returns `Some(uid)` if exactly one candidate, `None` if 0 or ≥2 (ambiguous).
fn unique_or_none(mut candidates: Vec<UserId>) -> Option<UserId> {
    candidates.sort_unstable();
    candidates.dedup();
    if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }
}

// ─── Guild cache entry ────────────────────────────────────────────────────────

struct GuildCacheEntry {
    /// user_id → roster entry.
    entries: HashMap<UserId, RosterEntry>,
    /// When the cache was last loaded from DB.
    loaded_at: Instant,
}

// ─── Per-thread speaker tracking ─────────────────────────────────────────────

/// Per-thread recorded speakers (scope C).
#[derive(Default)]
struct ThreadSpeakers {
    speakers: HashMap<u64, HashSet<UserId>>, // thread_id (u64) → speakers
}

// ─── RosterCache ─────────────────────────────────────────────────────────────

/// Runtime roster cache.
///
/// One instance should be held in `Arc<RosterCache>` and shared across handlers.
pub struct RosterCache {
    /// GuildId → loaded guild cache
    guilds: Arc<RwLock<HashMap<GuildId, GuildCacheEntry>>>,
    /// Per-thread speaker sets (scope C)
    speakers: Arc<RwLock<ThreadSpeakers>>,
    /// Cache TTL
    ttl: Duration,
    /// Whether heuristic resolution is enabled (always returns None even if true)
    heuristic_enabled: bool,
    /// korean_match_mode from config. "suffix_strip" enables stage 5; any other value skips it.
    korean_match_mode: String,
}

impl RosterCache {
    /// Create a new empty cache.
    pub fn new(ttl_secs: u64, heuristic_enabled: bool, korean_match_mode: String) -> Self {
        Self {
            guilds: Arc::new(RwLock::new(HashMap::new())),
            speakers: Arc::new(RwLock::new(ThreadSpeakers::default())),
            ttl: Duration::from_secs(ttl_secs),
            heuristic_enabled,
            korean_match_mode,
        }
    }

    // ── Speaker tracking (scope C) ─────────────────────────────────────────

    /// Record a message author in a thread (scope C update).
    pub async fn record_speaker(&self, thread_id: ChannelId, user_id: UserId) {
        let mut guard = self.speakers.write().await;
        guard
            .speakers
            .entry(thread_id.get())
            .or_default()
            .insert(user_id);
    }

    // ── Channel scope (A ∪ C) ─────────────────────────────────────────────

    /// Compute the channel scope for a thread: A (thread members) ∪ C (speakers).
    ///
    /// A is fetched via `ChannelId::get_thread_members(&http)`.
    /// Failures are logged and the partial result is still returned.
    pub async fn channel_scope(
        &self,
        thread_id: ChannelId,
        http: &Http,
    ) -> HashSet<UserId> {
        // A: thread join list
        let mut scope = HashSet::new();
        match thread_id.get_thread_members(http).await {
            Ok(members) => {
                for m in members {
                    scope.insert(m.user_id);
                }
            }
            Err(e) => {
                warn!(
                    thread_id = %thread_id,
                    "get_thread_members failed: {}. Falling back to speakers only.",
                    e
                );
            }
        }

        // C: speakers
        {
            let guard = self.speakers.read().await;
            if let Some(speakers) = guard.speakers.get(&thread_id.get()) {
                scope.extend(speakers.iter().copied());
            }
        }

        scope
    }

    // ── DB load ───────────────────────────────────────────────────────────

    /// Ensure guild entries are loaded (or reload if TTL expired).
    async fn ensure_loaded(&self, guild_id: GuildId, pool: &PgPool) {
        // fast path: check under read lock
        {
            let guard = self.guilds.read().await;
            if let Some(entry) = guard.get(&guild_id)
                && entry.loaded_at.elapsed() < self.ttl
            {
                return;
            }
        }

        // slow path: reload from DB
        match db_roster::list_guild_members(pool, guild_id.get() as i64).await {
            Ok(rows) => {
                let entries: HashMap<UserId, RosterEntry> = rows
                    .into_iter()
                    .map(|r| {
                        let uid = UserId::new(r.user_id as u64);
                        let entry = RosterEntry {
                            user_id: uid,
                            username: r.username,
                            global_name: r.global_name,
                            guild_nickname: r.guild_nickname,
                            aliases: r.aliases.0,
                        };
                        (uid, entry)
                    })
                    .collect();

                let mut guard = self.guilds.write().await;
                guard.insert(
                    guild_id,
                    GuildCacheEntry {
                        entries,
                        loaded_at: Instant::now(),
                    },
                );
            }
            Err(e) => {
                warn!(
                    guild_id = %guild_id,
                    "Failed to load roster from DB: {}. Using stale cache if available.",
                    e
                );
            }
        }
    }

    // ── Snapshot ──────────────────────────────────────────────────────────

    /// Take a point-in-time snapshot of the guild's roster.
    ///
    /// Should be called at turn start. The snapshot is then passed to
    /// `RosterSnapshot::resolve()` throughout the turn, protecting against
    /// member-leave races.
    ///
    /// If the cache is stale, this method triggers a DB reload before snapshotting.
    pub async fn snapshot(&self, guild_id: GuildId, pool: &PgPool) -> RosterSnapshot {
        self.ensure_loaded(guild_id, pool).await;

        let guard = self.guilds.read().await;
        let entries = guard
            .get(&guild_id)
            .map(|e| e.entries.clone())
            .unwrap_or_default();

        RosterSnapshot {
            entries,
            heuristic_enabled: self.heuristic_enabled,
            korean_match_mode: self.korean_match_mode.clone(),
        }
    }

    // ── Direct upsert/remove (for gateway events) ─────────────────────────

    /// Upsert a single member into the in-memory cache.
    ///
    /// Called by gateway event handlers (GuildMemberAdd / GuildMemberUpdate).
    /// Does not touch the DB — callers should call `db::roster::upsert_member` separately.
    pub async fn upsert_entry(&self, guild_id: GuildId, entry: RosterEntry) {
        let mut guard = self.guilds.write().await;
        if let Some(cache) = guard.get_mut(&guild_id) {
            cache.entries.insert(entry.user_id, entry);
        }
        // If the cache for this guild isn't loaded yet, we skip (will be loaded lazily).
    }

    /// Remove a member from the in-memory cache.
    ///
    /// Called by gateway event handlers (GuildMemberRemove).
    pub async fn remove_entry(&self, guild_id: GuildId, user_id: UserId) {
        let mut guard = self.guilds.write().await;
        if let Some(cache) = guard.get_mut(&guild_id) {
            cache.entries.remove(&user_id);
        }
    }

    /// Invalidate the entire guild cache (forces DB reload on next access).
    pub async fn invalidate(&self, guild_id: GuildId) {
        let mut guard = self.guilds.write().await;
        guard.remove(&guild_id);
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// NFC-normalize a string (same pattern as `handler::mention::nfc`).
pub(crate) fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Strip the longest matching Korean honorific suffix from `name_nfc`.
///
/// Only strips once (S4 approach 1). NFC must be applied before calling.
/// Returns `Some(stripped)` if a suffix was found, `None` otherwise.
fn strip_korean_suffix(name_nfc: &str) -> Option<String> {
    for suffix in KOREAN_SUFFIXES {
        if name_nfc.ends_with(suffix) && name_nfc.len() > suffix.len() {
            let stripped = &name_nfc[..name_nfc.len() - suffix.len()];
            if !stripped.is_empty() {
                return Some(stripped.to_string());
            }
        }
    }
    None
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn uid(n: u64) -> UserId {
        UserId::new(n)
    }

    fn make_entry(
        user_id: u64,
        username: &str,
        global_name: Option<&str>,
        guild_nickname: Option<&str>,
        aliases: &[&str],
    ) -> RosterEntry {
        RosterEntry {
            user_id: uid(user_id),
            username: username.to_string(),
            global_name: global_name.map(str::to_string),
            guild_nickname: guild_nickname.map(str::to_string),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn snapshot_from(entries: Vec<RosterEntry>, heuristic_enabled: bool) -> RosterSnapshot {
        snapshot_from_mode(entries, heuristic_enabled, "suffix_strip")
    }

    fn snapshot_from_mode(entries: Vec<RosterEntry>, heuristic_enabled: bool, mode: &str) -> RosterSnapshot {
        let map: HashMap<UserId, RosterEntry> =
            entries.into_iter().map(|e| (e.user_id, e.clone())).collect();
        RosterSnapshot {
            entries: map,
            heuristic_enabled,
            korean_match_mode: mode.to_string(),
        }
    }

    fn scope(uids: &[u64]) -> HashSet<UserId> {
        uids.iter().copied().map(uid).collect()
    }

    // ── strip_korean_suffix ───────────────────────────────────────────────────

    #[test]
    fn strip_suffix_이형() {
        assert_eq!(strip_korean_suffix("재민이형"), Some("재민".to_string()));
    }

    #[test]
    fn strip_suffix_형() {
        assert_eq!(strip_korean_suffix("재민형"), Some("재민".to_string()));
    }

    #[test]
    fn strip_suffix_이님() {
        assert_eq!(strip_korean_suffix("재민이님"), Some("재민".to_string()));
    }

    #[test]
    fn strip_suffix_님() {
        assert_eq!(strip_korean_suffix("재민님"), Some("재민".to_string()));
    }

    #[test]
    fn strip_suffix_이씨() {
        assert_eq!(strip_korean_suffix("홍길이씨"), Some("홍길".to_string()));
    }

    #[test]
    fn strip_suffix_씨() {
        assert_eq!(strip_korean_suffix("홍길씨"), Some("홍길".to_string()));
    }

    #[test]
    fn strip_suffix_아() {
        assert_eq!(strip_korean_suffix("민수아"), Some("민수".to_string()));
    }

    #[test]
    fn strip_suffix_야() {
        assert_eq!(strip_korean_suffix("민수야"), Some("민수".to_string()));
    }

    #[test]
    fn strip_suffix_이() {
        // "이" 단독 접미 — 가장 낮은 우선순위
        assert_eq!(strip_korean_suffix("재민이"), Some("재민".to_string()));
    }

    #[test]
    fn strip_no_match() {
        assert_eq!(strip_korean_suffix("재민"), None);
        assert_eq!(strip_korean_suffix("Mark"), None);
        assert_eq!(strip_korean_suffix(""), None);
    }

    #[test]
    fn strip_longest_match_priority() {
        // "이형" is longer than "이" — should strip "이형", not just "이"
        let result = strip_korean_suffix("재민이형");
        assert_eq!(result, Some("재민".to_string()));
        // If it incorrectly stripped "이" first, result would be "재민이형"[..-"이".len()] = "재민이형"?
        // No: "재민이형".ends_with("이") is true, but "이형" comes first in KOREAN_SUFFIXES
        // so the loop hits "이형" before "이".
    }

    #[test]
    fn strip_suffix_only_string_returns_none() {
        // The suffix IS the entire string — no base name left → None
        assert_eq!(strip_korean_suffix("형"), None);
        assert_eq!(strip_korean_suffix("님"), None);
    }

    // ── resolve — stage 1: username exact ────────────────────────────────────

    #[test]
    fn resolve_username_exact() {
        let snap = snapshot_from(
            vec![make_entry(1, "jaemin", None, None, &[])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("jaemin", &sc), Some(uid(1)));
    }

    #[test]
    fn resolve_username_miss() {
        let snap = snapshot_from(
            vec![make_entry(1, "jaemin", None, None, &[])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("unknown", &sc), None);
    }

    // ── resolve — stage 2: guild_nickname ────────────────────────────────────

    #[test]
    fn resolve_guild_nickname() {
        let snap = snapshot_from(
            vec![make_entry(1, "user1", None, Some("재민"), &[])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("재민", &sc), Some(uid(1)));
    }

    #[test]
    fn resolve_nickname_over_username_priority() {
        // username doesn't match, but nickname does
        let snap = snapshot_from(
            vec![make_entry(1, "jaemin_kr", None, Some("재민"), &[])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("재민", &sc), Some(uid(1)));
        // username doesn't match "재민"
        assert_eq!(snap.resolve("jaemin_kr", &sc), Some(uid(1)));
    }

    // ── resolve — stage 3: global_name ───────────────────────────────────────

    #[test]
    fn resolve_global_name() {
        let snap = snapshot_from(
            vec![make_entry(1, "user1", Some("JaeMin Global"), None, &[])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("JaeMin Global", &sc), Some(uid(1)));
    }

    // ── resolve — stage 4: alias ──────────────────────────────────────────────

    #[test]
    fn resolve_alias() {
        let snap = snapshot_from(
            vec![make_entry(1, "user1", None, None, &["재민", "jm"])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("재민", &sc), Some(uid(1)));
        assert_eq!(snap.resolve("jm", &sc), Some(uid(1)));
    }

    // ── resolve — ambiguous (동명이인) → None ─────────────────────────────────

    #[test]
    fn resolve_ambiguous_username_returns_none() {
        // Two users share the same username (shouldn't happen in practice, but guard it)
        let snap = snapshot_from(
            vec![
                make_entry(1, "twin", None, None, &[]),
                make_entry(2, "twin", None, None, &[]),
            ],
            false,
        );
        let sc = scope(&[1, 2]);
        // Ambiguous → None
        assert_eq!(snap.resolve("twin", &sc), None);
    }

    #[test]
    fn resolve_ambiguous_nickname_returns_none() {
        let snap = snapshot_from(
            vec![
                make_entry(1, "user_a", None, Some("민수"), &[]),
                make_entry(2, "user_b", None, Some("민수"), &[]),
            ],
            false,
        );
        let sc = scope(&[1, 2]);
        assert_eq!(snap.resolve("민수", &sc), None);
    }

    // ── resolve — scope exclusion ────────────────────────────────────────────

    #[test]
    fn resolve_out_of_scope_returns_none() {
        let snap = snapshot_from(
            vec![make_entry(1, "jaemin", None, None, &[])],
            false,
        );
        // Scope does NOT include user 1
        let sc = scope(&[99]);
        assert_eq!(snap.resolve("jaemin", &sc), None);
    }

    #[test]
    fn resolve_empty_scope_returns_none() {
        let snap = snapshot_from(
            vec![make_entry(1, "jaemin", None, None, &[])],
            false,
        );
        assert_eq!(snap.resolve("jaemin", &HashSet::new()), None);
    }

    // ── resolve — heuristic OFF → always None ─────────────────────────────────

    #[test]
    fn heuristic_off_returns_none() {
        // Even with a plausible name, heuristic=false means no UserId from heuristic
        let snap = snapshot_from(vec![], false);
        let sc = scope(&[1, 2, 3]);
        assert_eq!(snap.resolve("SomeName", &sc), None);
    }

    #[test]
    fn heuristic_on_still_returns_none() {
        // heuristic=true but per spec it MUST NOT return UserId — None is the only valid result
        let snap = snapshot_from(vec![], true);
        let sc = scope(&[1, 2, 3]);
        assert_eq!(snap.resolve("SomeName", &sc), None);
    }

    // ── resolve — Korean suffix strip ────────────────────────────────────────

    #[test]
    fn resolve_korean_suffix_이형() {
        let snap = snapshot_from(
            vec![make_entry(1, "user1", None, Some("재민"), &[])],
            false,
        );
        let sc = scope(&[1]);
        // "재민이형" → strip "이형" → "재민" → guild_nickname match
        assert_eq!(snap.resolve("재민이형", &sc), Some(uid(1)));
    }

    #[test]
    fn resolve_korean_suffix_님() {
        let snap = snapshot_from(
            vec![make_entry(1, "user1", None, Some("재민"), &[])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("재민님", &sc), Some(uid(1)));
    }

    #[test]
    fn resolve_korean_suffix_야() {
        let snap = snapshot_from(
            vec![make_entry(1, "minsoo", None, None, &["민수"])],
            false,
        );
        let sc = scope(&[1]);
        // "민수야" → strip "야" → "민수" → alias match
        assert_eq!(snap.resolve("민수야", &sc), Some(uid(1)));
    }

    #[test]
    fn resolve_korean_suffix_out_of_scope_returns_none() {
        let snap = snapshot_from(
            vec![make_entry(1, "user1", None, Some("재민"), &[])],
            false,
        );
        // User 1 is NOT in scope
        let sc = scope(&[99]);
        assert_eq!(snap.resolve("재민이형", &sc), None);
    }

    // ── resolve — NFC normalization ───────────────────────────────────────────

    #[test]
    fn resolve_nfc_decomposed_input() {
        // Cache stores NFC "덕돌". Input is NFD-decomposed jamo.
        let snap = snapshot_from(
            vec![make_entry(1, "deokdol", Some("덕돌"), None, &[])],
            false,
        );
        let sc = scope(&[1]);
        // NFD-decomposed "덕돌"
        let nfd = "\u{1103}\u{1165}\u{11A8}\u{1103}\u{1169}\u{11AF}";
        assert_eq!(snap.resolve(nfd, &sc), Some(uid(1)));
    }

    // ── resolve — priority order (username > nickname > global_name > alias) ──

    #[test]
    fn resolve_priority_username_beats_nickname() {
        // user A: username="재민", no nickname
        // user B: username="other", nickname="재민"
        // Both in scope. Username stage resolves user A uniquely.
        let snap = snapshot_from(
            vec![
                make_entry(1, "재민", None, None, &[]),
                make_entry(2, "other", None, Some("재민"), &[]),
            ],
            false,
        );
        let sc = scope(&[1, 2]);
        // At stage 1 (username), only user 1 matches → returns uid(1).
        // user 2 also has nickname "재민" but stage 1 resolves uniquely first.
        assert_eq!(snap.resolve("재민", &sc), Some(uid(1)));
    }

    #[test]
    fn resolve_priority_nickname_beats_global_name() {
        // user A: global_name="재민", no nickname
        // user B: global_name="other_global", nickname="재민"
        let snap = snapshot_from(
            vec![
                make_entry(1, "user_a", Some("재민"), None, &[]),
                make_entry(2, "user_b", Some("other_global"), Some("재민"), &[]),
            ],
            false,
        );
        let sc = scope(&[1, 2]);
        // Stage 2 (nickname): only user 2 has "재민" nickname → uid(2)
        assert_eq!(snap.resolve("재민", &sc), Some(uid(2)));
    }

    // ── resolve — priority: global_name beats alias ───────────────────────────

    #[test]
    fn resolve_priority_global_name_beats_alias() {
        // user A: global_name="재민", no alias
        // user B: global_name="other", alias="재민"
        // Stage 3 (global_name) resolves user A uniquely before alias stage.
        let snap = snapshot_from(
            vec![
                make_entry(1, "user_a", Some("재민"), None, &[]),
                make_entry(2, "user_b", Some("other"), None, &["재민"]),
            ],
            false,
        );
        let sc = scope(&[1, 2]);
        // Stage 3 (global_name): only user 1 has global_name "재민" → uid(1)
        assert_eq!(snap.resolve("재민", &sc), Some(uid(1)));
    }

    // ── resolve — Korean suffix strip edge cases ──────────────────────────────

    #[test]
    fn resolve_korean_suffix_씨() {
        // @민수씨 → strip "씨" → "민수" → match via guild_nickname
        let snap = snapshot_from(
            vec![make_entry(1, "minsoo", None, Some("민수"), &[])],
            false,
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("민수씨", &sc), Some(uid(1)));
    }

    #[test]
    fn resolve_no_suffix_no_match_returns_none() {
        // "재밍" has no strippable suffix AND is not in roster → None
        let snap = snapshot_from(
            vec![make_entry(1, "jaemin", None, Some("재민"), &[])],
            false,
        );
        let sc = scope(&[1]);
        // "재밍" ≠ "재민", no suffix to strip → resolve returns None
        assert_eq!(snap.resolve("재밍", &sc), None);
    }

    // ── korean_match_mode wiring ──────────────────────────────────────────────

    #[test]
    fn korean_match_mode_suffix_strip_resolves() {
        // mode="suffix_strip" → @재민이형 strips to "재민" → guild_nickname match
        let snap = snapshot_from_mode(
            vec![make_entry(1, "user1", None, Some("재민"), &[])],
            false,
            "suffix_strip",
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("재민이형", &sc), Some(uid(1)));
    }

    #[test]
    fn korean_match_mode_exact_skips_strip() {
        // mode="exact" → strip stage skipped → @재민이형 has no exact match → None
        let snap = snapshot_from_mode(
            vec![make_entry(1, "user1", None, Some("재민"), &[])],
            false,
            "exact",
        );
        let sc = scope(&[1]);
        // strip skipped — "재민이형" is not an exact match for "재민"
        assert_eq!(snap.resolve("재민이형", &sc), None);
        // but the exact name still resolves
        assert_eq!(snap.resolve("재민", &sc), Some(uid(1)));
    }

    #[test]
    fn korean_match_mode_off_skips_strip() {
        // mode="off" → strip stage skipped
        let snap = snapshot_from_mode(
            vec![make_entry(1, "user1", None, Some("재민"), &[])],
            false,
            "off",
        );
        let sc = scope(&[1]);
        assert_eq!(snap.resolve("재민이형", &sc), None);
        assert_eq!(snap.resolve("재민", &sc), Some(uid(1)));
    }

    // ── whitelist hallucination guard: scope-outside user never in whitelist ──

    #[test]
    fn roster_whitelist_excludes_out_of_scope_user() {
        // Roster has user 1 (username="jaemin"), but user 1 is NOT in scope.
        // @jaemin → resolve returns None (out of scope) → whitelist stays empty.
        use crate::handler::mention::replace_mentions_with_roster;

        let snap = snapshot_from(
            vec![make_entry(1, "jaemin", None, None, &[])],
            false,
        );
        // Scope intentionally excludes user 1
        let sc = scope(&[99]);
        let (out, whitelist) = replace_mentions_with_roster("@jaemin 안녕", &snap, &sc);
        // No substitution — "@jaemin" preserved
        assert_eq!(out, "@jaemin 안녕");
        // Whitelist must be empty — no accidental ping of out-of-scope user
        assert!(whitelist.is_empty());
    }

    #[test]
    fn roster_whitelist_includes_only_resolved_users() {
        // Two users in roster, only user 1 is in scope.
        // @user1 matches and is in scope → in whitelist.
        // @user2 matches roster but out of scope → NOT in whitelist.
        use crate::handler::mention::replace_mentions_with_roster;

        let snap = snapshot_from(
            vec![
                make_entry(1, "user1", None, None, &[]),
                make_entry(2, "user2", None, None, &[]),
            ],
            false,
        );
        // Only user1 in scope
        let sc = scope(&[1]);
        let (out, whitelist) = replace_mentions_with_roster("@user1 @user2", &snap, &sc);
        // user1 substituted, user2 left as-is
        assert!(out.contains("<@1>"));
        assert!(out.contains("@user2"));
        // Only user1 in whitelist
        assert_eq!(whitelist, vec![uid(1)]);
    }

    // ── heuristic ON — all exact stages miss, suffix strip misses → still None ─

    #[test]
    fn heuristic_on_with_exact_miss_and_no_suffix_returns_none() {
        // heuristic_enabled=true but the heuristic stage is a no-op guard.
        // Even if all prior stages fail, result is None — never a UserId.
        let snap = snapshot_from(
            vec![make_entry(1, "jaemin", None, Some("재민"), &["jm"])],
            true,
        );
        let sc = scope(&[1]);
        // "unknown" ≠ any field, no strip → None
        assert_eq!(snap.resolve("unknown", &sc), None);
        // "재밍" ≠ "재민", strip "이" → "재밍"[..-"이".len()] but "재밍" doesn't end with "이"
        // Actually strip_korean_suffix("재밍") → None → no strip → None
        assert_eq!(snap.resolve("재밍", &sc), None);
    }

    // ── unique_or_none ────────────────────────────────────────────────────────

    #[test]
    fn unique_or_none_single() {
        assert_eq!(unique_or_none(vec![uid(1)]), Some(uid(1)));
    }

    #[test]
    fn unique_or_none_empty() {
        assert_eq!(unique_or_none(vec![]), None);
    }

    #[test]
    fn unique_or_none_duplicate() {
        // same uid duplicated (shouldn't happen but must dedup correctly)
        assert_eq!(unique_or_none(vec![uid(1), uid(1)]), Some(uid(1)));
    }

    #[test]
    fn unique_or_none_two_different() {
        assert_eq!(unique_or_none(vec![uid(1), uid(2)]), None);
    }

    // ── record_speaker / channel_scope (unit, no HTTP) ─────────────────────

    #[tokio::test]
    async fn record_speaker_stored() {
        let cache = RosterCache::new(300, false, "suffix_strip".to_string());
        let thread = ChannelId::new(10);
        let user = uid(42);
        cache.record_speaker(thread, user).await;

        let guard = cache.speakers.read().await;
        let speakers = guard.speakers.get(&10).unwrap();
        assert!(speakers.contains(&user));
    }

    #[tokio::test]
    async fn record_multiple_speakers() {
        let cache = RosterCache::new(300, false, "suffix_strip".to_string());
        let thread = ChannelId::new(20);
        cache.record_speaker(thread, uid(1)).await;
        cache.record_speaker(thread, uid(2)).await;
        cache.record_speaker(thread, uid(1)).await; // duplicate

        let guard = cache.speakers.read().await;
        let speakers = guard.speakers.get(&20).unwrap();
        assert_eq!(speakers.len(), 2);
        assert!(speakers.contains(&uid(1)));
        assert!(speakers.contains(&uid(2)));
    }

    // ── upsert_entry / remove_entry ───────────────────────────────────────────

    #[tokio::test]
    async fn upsert_and_remove_entry() {
        let cache = RosterCache::new(300, false, "suffix_strip".to_string());
        let guild = GuildId::new(100);
        // Pre-populate the guild cache so upsert works
        {
            let mut guard = cache.guilds.write().await;
            guard.insert(
                guild,
                GuildCacheEntry {
                    entries: HashMap::new(),
                    loaded_at: Instant::now(),
                },
            );
        }

        let entry = make_entry(1, "jaemin", None, Some("재민"), &[]);
        cache.upsert_entry(guild, entry).await;

        {
            let guard = cache.guilds.read().await;
            let entries = &guard.get(&guild).unwrap().entries;
            assert!(entries.contains_key(&uid(1)));
        }

        cache.remove_entry(guild, uid(1)).await;
        {
            let guard = cache.guilds.read().await;
            let entries = &guard.get(&guild).unwrap().entries;
            assert!(!entries.contains_key(&uid(1)));
        }
    }

    // ── invalidate ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn invalidate_clears_guild() {
        let cache = RosterCache::new(300, false, "suffix_strip".to_string());
        let guild = GuildId::new(200);
        {
            let mut guard = cache.guilds.write().await;
            guard.insert(
                guild,
                GuildCacheEntry {
                    entries: HashMap::new(),
                    loaded_at: Instant::now(),
                },
            );
        }
        cache.invalidate(guild).await;
        {
            let guard = cache.guilds.read().await;
            assert!(!guard.contains_key(&guild));
        }
    }
}
