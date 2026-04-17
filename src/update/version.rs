pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn parse_tag(tag: &str) -> Option<[u32; 3]> {
    let s = if tag.starts_with('v') || tag.starts_with('V') {
        &tag[1..]
    } else {
        tag
    };

    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let a = parts[0].parse::<u32>().ok()?;
    let b = parts[1].parse::<u32>().ok()?;
    let c = parts[2].parse::<u32>().ok()?;

    Some([a, b, c])
}

pub fn needs_update(current: &str, latest_tag: &str, force: bool) -> bool {
    if force {
        return true;
    }

    let current_ver = match parse_tag(current) {
        Some(v) => v,
        None => return false,
    };

    let latest_ver = match parse_tag(latest_tag) {
        Some(v) => v,
        None => return false,
    };

    latest_ver > current_ver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_with_v_prefix() {
        assert_eq!(parse_tag("v0.6.4"), Some([0, 6, 4]));
    }

    #[test]
    fn parse_tag_without_prefix() {
        assert_eq!(parse_tag("0.6.4"), Some([0, 6, 4]));
    }

    #[test]
    fn parse_tag_with_capital_v_prefix() {
        assert_eq!(parse_tag("V0.6.4"), Some([0, 6, 4]));
    }

    #[test]
    fn parse_tag_malformed_string() {
        assert_eq!(parse_tag("abc"), None);
    }

    #[test]
    fn parse_tag_too_few_parts() {
        assert_eq!(parse_tag("0.6"), None);
    }

    #[test]
    fn parse_tag_too_many_parts() {
        assert_eq!(parse_tag("0.6.4.1"), None);
    }

    #[test]
    fn needs_update_newer_available() {
        assert!(needs_update("0.6.3", "v0.6.4", false));
    }

    #[test]
    fn needs_update_same_version() {
        assert!(!needs_update("0.6.4", "v0.6.4", false));
    }

    #[test]
    fn needs_update_force_flag() {
        assert!(needs_update("0.6.4", "v0.6.4", true));
    }

    #[test]
    fn needs_update_downgrade() {
        assert!(!needs_update("0.6.5", "v0.6.4", false));
    }

    #[test]
    fn needs_update_semver_numeric_comparison() {
        assert!(needs_update("0.6.3", "v0.6.10", false));
    }

    #[test]
    fn needs_update_malformed_current() {
        assert!(!needs_update("abc", "v0.6.4", false));
    }
}
