//! Deduplication logic for permission rules (canonical form normalization).
// TODO(#286 T4): Implement canonical form normalization and dedup for permissions.allow array

use serde_json::Value;

/// Result of attempting to merge a rule into the allow list.
#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub(crate) enum MergeAction {
    /// Rule was not present; it has been appended.
    Added,
    /// Rule was already present; allow list is unchanged.
    AlreadyPresent,
}

/// Normalize a permission rule to its canonical form.
///
/// Transformations applied:
/// 1. `WebFetch(http[s]://...)` → `WebFetch(domain:<host>)`. WebFetch는 이 단계
///    하나만 적용하며, trailing wildcard 정규화는 건너뛴다 (review #295 s2 fix).
///    이미 정규화된 `WebFetch(domain:...)`는 그대로 반환. host 추출 실패 시 입력 그대로.
/// 2. 나머지 rule에 대해 trailing `:*` just before closing `)` → ` *`
///    e.g. `Bash(npm:*)` → `Bash(npm *)`, `Bash(npm arg:*)` → `Bash(npm arg *)`
///    Middle `:*` (not immediately before `)`)는 손대지 않는다.
/// 3. path namespace 4종 (`./`, `//`, `~/`, `/`) 등은 변경 없음.
///
/// WebFetch에 trailing wildcard normalize를 적용하지 않는 이유:
/// `WebFetch(https://example.com:*)` 같은 입력이 들어오면 `:*`가 URL의 일부로
/// 잘못 해석돼 `WebFetch(domain:example.com *)` 같은 부정확한 결과를 만들었다.
#[allow(dead_code)]
pub(crate) fn normalize_rule(input: &str) -> String {
    if input.starts_with("WebFetch(") {
        return normalize_webfetch_url(input);
    }
    normalize_trailing_wildcard(input)
}

/// Replace trailing `:*)` with ` *)` at the end of the string.
///
/// Only the `:` immediately before `*)` at the very end is replaced.
/// Middle occurrences of `:*` are not touched.
fn normalize_trailing_wildcard(input: &str) -> String {
    // The rule ends with ":*)" — strip ")", check ":*" suffix, replace ":" → " "
    if let Some(without_paren) = input.strip_suffix(')')
        && let Some(before_star) = without_paren.strip_suffix(":*")
    {
        // Only normalize if there is content before ":*" (i.e. not an empty arg)
        if !before_star.is_empty() {
            return format!("{} *)", before_star);
        }
    }
    input.to_string()
}

/// Extract the host from a `WebFetch(http[s]://...)` rule and return
/// `WebFetch(domain:<host>)`.  Returns the input unchanged on any failure.
fn normalize_webfetch_url(input: &str) -> String {
    // Only act on WebFetch( prefix
    let Some(after_prefix) = input.strip_prefix("WebFetch(") else {
        return input.to_string();
    };

    // Already normalized
    if after_prefix.starts_with("domain:") {
        return input.to_string();
    }

    // Must end with ')'
    let Some(url_part) = after_prefix.strip_suffix(')') else {
        return input.to_string();
    };

    // Extract host from http[s]://host/...
    // Supported patterns: "http://" or "https://"
    let after_scheme = if let Some(rest) = url_part.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url_part.strip_prefix("http://") {
        rest
    } else {
        // Not an http/https URL — return unchanged
        return input.to_string();
    };

    // Host ends at first '/', '?', '#', or end of string
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);

    if host.is_empty() {
        return input.to_string();
    }

    format!("WebFetch(domain:{})", host)
}

/// Merge `normalized_rule` into the `allow` array (dedup by exact string equality).
///
/// Non-string entries in `allow` are ignored and preserved (safety: never corrupt
/// user settings that contain unexpected JSON).
///
/// Returns `MergeAction::AlreadyPresent` if an exact match is found;
/// otherwise appends and returns `MergeAction::Added`.
#[allow(dead_code)]
pub(crate) fn merge_into_allow(allow: &mut Vec<Value>, normalized_rule: String) -> MergeAction {
    for entry in allow.iter() {
        if let Value::String(s) = entry
            && s == &normalized_rule
        {
            return MergeAction::AlreadyPresent;
        }
    }
    allow.push(Value::String(normalized_rule));
    MergeAction::Added
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // 1. trailing wildcard normalization: Bash(npm:*) → Bash(npm *)
    #[test]
    fn trailing_wildcard_colon_normalized() {
        assert_eq!(normalize_rule("Bash(npm:*)"), "Bash(npm *)");
    }

    // 2. already canonical form unchanged
    #[test]
    fn trailing_wildcard_already_canonical() {
        assert_eq!(normalize_rule("Bash(npm *)"), "Bash(npm *)");
    }

    // 3. start-position preserved: Bash(npm arg:*) → Bash(npm arg *)
    #[test]
    fn trailing_wildcard_with_prefix_args() {
        assert_eq!(normalize_rule("Bash(npm arg:*)"), "Bash(npm arg *)");
    }

    // 4. no wildcard arg → unchanged
    #[test]
    fn no_wildcard_unchanged() {
        assert_eq!(normalize_rule("Bash(npm)"), "Bash(npm)");
    }

    // 5. path namespace 4종 보존 (./  //  ~/  /)
    #[test]
    fn path_namespace_preserved() {
        let cases = [
            "Read(./src/**)",
            "Write(//network/share)",
            "Bash(~/scripts/run.sh)",
            "Edit(/etc/config)",
        ];
        for case in &cases {
            assert_eq!(normalize_rule(case), *case, "path namespace changed: {}", case);
        }
    }

    // 6. URL → domain extraction
    #[test]
    fn url_to_domain() {
        assert_eq!(
            normalize_rule("WebFetch(https://example.com/path?q=1)"),
            "WebFetch(domain:example.com)"
        );
    }

    // 7. duplicate add → AlreadyPresent, vec length stays 1
    #[test]
    fn merge_duplicate_already_present() {
        let mut allow = vec![json!("Bash(npm *)")];
        let action = merge_into_allow(&mut allow, "Bash(npm *)".to_string());
        assert_eq!(action, MergeAction::AlreadyPresent);
        assert_eq!(allow.len(), 1);
    }

    // 8. new rule add → Added, vec length becomes 1
    #[test]
    fn merge_new_rule_added() {
        let mut allow: Vec<Value> = vec![];
        let action = merge_into_allow(&mut allow, "Bash(ls)".to_string());
        assert_eq!(action, MergeAction::Added);
        assert_eq!(allow.len(), 1);
        assert_eq!(allow[0], json!("Bash(ls)"));
    }

    // Fuzz/random: same rule added 100 times → final length 1 (dedup correctness)
    #[test]
    fn fuzz_same_rule_100_times_dedup() {
        let rule = "Bash(npm *)";
        let mut allow: Vec<Value> = vec![];
        for _ in 0..100 {
            merge_into_allow(&mut allow, rule.to_string());
        }
        assert_eq!(allow.len(), 1, "dedup failed: {} entries after 100 inserts", allow.len());
    }

    // Extra: already-domain form unchanged
    #[test]
    fn webfetch_already_domain_unchanged() {
        assert_eq!(
            normalize_rule("WebFetch(domain:example.com)"),
            "WebFetch(domain:example.com)"
        );
    }

    // Extra: non-string entries in allow are preserved
    #[test]
    fn merge_preserves_non_string_entries() {
        let mut allow = vec![json!({"key": "value"}), json!("Bash(ls)")];
        let action = merge_into_allow(&mut allow, "Bash(pwd)".to_string());
        assert_eq!(action, MergeAction::Added);
        assert_eq!(allow.len(), 3);
        // non-string object is still there
        assert_eq!(allow[0], json!({"key": "value"}));
    }
}
