use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VersionError {
    #[error("malformed current version: {0}")]
    MalformedCurrent(String),
    #[error("malformed latest tag: {0}")]
    MalformedLatest(String),
}

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

pub fn needs_update(current: &str, latest_tag: &str, force: bool) -> Result<bool, VersionError> {
    if force {
        return Ok(true);
    }

    let current_ver =
        parse_tag(current).ok_or_else(|| VersionError::MalformedCurrent(current.to_string()))?;
    let latest_ver = parse_tag(latest_tag)
        .ok_or_else(|| VersionError::MalformedLatest(latest_tag.to_string()))?;

    Ok(latest_ver > current_ver)
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
        assert_eq!(needs_update("0.6.3", "v0.6.4", false), Ok(true));
    }

    #[test]
    fn needs_update_same_version() {
        assert_eq!(needs_update("0.6.4", "v0.6.4", false), Ok(false));
    }

    #[test]
    fn needs_update_force_flag() {
        assert_eq!(needs_update("0.6.4", "v0.6.4", true), Ok(true));
    }

    #[test]
    fn needs_update_downgrade() {
        assert_eq!(needs_update("0.6.5", "v0.6.4", false), Ok(false));
    }

    #[test]
    fn needs_update_semver_numeric_comparison() {
        assert_eq!(needs_update("0.6.3", "v0.6.10", false), Ok(true));
    }

    #[test]
    fn needs_update_malformed_current() {
        assert_eq!(
            needs_update("abc", "v0.6.4", false),
            Err(VersionError::MalformedCurrent("abc".to_string()))
        );
    }

    #[test]
    fn needs_update_malformed_latest() {
        assert_eq!(
            needs_update("0.6.3", "garbage", false),
            Err(VersionError::MalformedLatest("garbage".to_string()))
        );
    }
}
