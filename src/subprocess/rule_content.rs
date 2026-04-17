use url::Url;

pub fn derive_rule_content(tool_name: &str, input: &serde_json::Value) -> Option<(String, String)> {
    match tool_name {
        "Bash" => {
            let cmd = input.get("command")?.as_str()?;
            Some(("Bash".to_string(), cmd.to_string()))
        }
        "WebFetch" => {
            let url_str = input.get("url")?.as_str()?;
            let parsed = Url::parse(url_str).ok()?;
            let host = parsed.host_str()?;
            Some(("WebFetch".to_string(), format!("domain:{}", host)))
        }
        "Read" => {
            let path = input.get("file_path")?.as_str()?;
            Some(("Read".to_string(), path.to_string()))
        }
        "Edit" => {
            let path = input.get("file_path")?.as_str()?;
            Some(("Edit".to_string(), path.to_string()))
        }
        "Write" => {
            let path = input.get("file_path")?.as_str()?;
            Some(("Write".to_string(), path.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_bash_command() {
        let input = serde_json::json!({"command": "npm run test"});
        let result = derive_rule_content("Bash", &input);
        assert_eq!(result, Some(("Bash".to_string(), "npm run test".to_string())));
    }

    #[test]
    fn derive_webfetch_host() {
        let input = serde_json::json!({"url": "https://example.com/foo"});
        let result = derive_rule_content("WebFetch", &input);
        assert_eq!(result, Some(("WebFetch".to_string(), "domain:example.com".to_string())));
    }

    #[test]
    fn derive_webfetch_host_with_port() {
        let input = serde_json::json!({"url": "https://example.com:8443/foo"});
        let result = derive_rule_content("WebFetch", &input);
        assert_eq!(result, Some(("WebFetch".to_string(), "domain:example.com".to_string())));
    }

    #[test]
    fn derive_read_path() {
        let input = serde_json::json!({"file_path": "/tmp/foo.txt"});
        let result = derive_rule_content("Read", &input);
        assert_eq!(result, Some(("Read".to_string(), "/tmp/foo.txt".to_string())));
    }

    #[test]
    fn derive_unknown_tool_returns_none() {
        let input = serde_json::json!({"pattern": "*.rs"});
        let result = derive_rule_content("Grep", &input);
        assert_eq!(result, None);
    }

    #[test]
    fn derive_webfetch_malformed_url_returns_none() {
        let input = serde_json::json!({"url": "not-a-url"});
        let result = derive_rule_content("WebFetch", &input);
        assert_eq!(result, None);
    }
}
