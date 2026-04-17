use serde_json::Value;

#[allow(dead_code)]
pub fn build_control_response_allow(request_id: &str, input: &Value) -> String {
    let response = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "allow",
                "updatedInput": input
            }
        }
    });
    format!("{}\n", response)
}

#[allow(dead_code)]
pub fn build_control_response_deny(request_id: &str, message: &str) -> String {
    let response = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "deny",
                "message": message
            }
        }
    });
    format!("{}\n", response)
}

pub fn build_control_response_ask_answer(
    request_id: &str,
    input: &Value,
    answers: &std::collections::HashMap<String, String>,
) -> String {
    let mut updated_input = input.clone();
    if let Value::Object(ref mut map) = updated_input {
        let answers_value: serde_json::Map<String, Value> = answers
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        map.insert("answers".to_string(), Value::Object(answers_value));
    }
    let json = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "allow",
                "updatedInput": updated_input
            }
        }
    });
    format!("{}\n", json)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeMode {
    None,
    Bogus,
    Real,
}

impl ProbeMode {
    pub fn from_env() -> Self {
        match std::env::var("PIDORY_SPIKE_PROBE").as_deref() {
            Ok("bogus") => Self::Bogus,
            Ok("real") => Self::Real,
            _ => Self::None,
        }
    }
}

/// Spike-only: builds allow response with optional probe fields.
/// ProbeMode::None — identical to build_control_response_allow.
/// ProbeMode::Bogus — adds __pidory_probe: true to response (tests schema tolerance).
/// ProbeMode::Real — adds updatedPermissions using derive_rule_content; omits field if None.
#[allow(dead_code)]
pub fn build_control_response_allow_probed(
    request_id: &str,
    input: &Value,
    probe_mode: &ProbeMode,
    tool_name: &str,
) -> String {
    let mut response_inner = serde_json::json!({
        "behavior": "allow",
        "updatedInput": input
    });

    match probe_mode {
        ProbeMode::None => {}
        ProbeMode::Bogus => {
            response_inner["__pidory_probe"] = serde_json::Value::Bool(true);
        }
        ProbeMode::Real => {
            if let Some((tool, rule)) = crate::subprocess::rule_content::derive_rule_content(tool_name, input) {
                response_inner["updatedPermissions"] = serde_json::json!([{
                    "type": "addRules",
                    "rules": [{
                        "toolName": tool,
                        "ruleContent": rule,
                    }],
                    "behavior": "allow",
                    "destination": "session"
                }]);
            }
            // derive_rule_content 가 None 이면 updatedPermissions 필드 생략 (backward-safe)
        }
    }

    let response = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response_inner
        }
    });
    format!("{}\n", response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_allow_response_format() {
        let input = serde_json::json!({"file_path": "/tmp/test.txt", "content": "hello"});
        let resp = build_control_response_allow("req_123", &input);
        let parsed: serde_json::Value = serde_json::from_str(resp.trim()).unwrap();
        assert_eq!(parsed["type"], "control_response");
        assert_eq!(parsed["response"]["subtype"], "success");
        assert_eq!(parsed["response"]["request_id"], "req_123");
        assert_eq!(parsed["response"]["response"]["behavior"], "allow");
        assert_eq!(parsed["response"]["response"]["updatedInput"]["file_path"], "/tmp/test.txt");
    }

    #[test]
    fn build_deny_response_format() {
        let resp = build_control_response_deny("req_456", "User rejected");
        let parsed: serde_json::Value = serde_json::from_str(resp.trim()).unwrap();
        assert_eq!(parsed["type"], "control_response");
        assert_eq!(parsed["response"]["subtype"], "success");
        assert_eq!(parsed["response"]["request_id"], "req_456");
        assert_eq!(parsed["response"]["response"]["behavior"], "deny");
        assert_eq!(parsed["response"]["response"]["message"], "User rejected");
    }

    #[test]
    fn build_allow_response_ends_with_newline() {
        let input = serde_json::json!({});
        let resp = build_control_response_allow("r", &input);
        assert!(resp.ends_with('\n'));
    }

    #[test]
    fn build_deny_response_ends_with_newline() {
        let resp = build_control_response_deny("r", "no");
        assert!(resp.ends_with('\n'));
    }
}

#[cfg(test)]
mod ask_answer_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn build_control_response_ask_answer_sets_q0() {
        let input = serde_json::json!({"questions": [{"question": "pick?"}]});
        let answers = HashMap::from([("q_0".to_string(), "Blue".to_string())]);
        let out = build_control_response_ask_answer("req-1", &input, &answers);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["response"]["response"]["updatedInput"]["answers"]["q_0"], "Blue");
    }

    #[test]
    fn build_control_response_ask_answer_multiple_questions() {
        let input = serde_json::json!({"questions": [{"question": "q1?"}, {"question": "q2?"}]});
        let answers = HashMap::from([
            ("q_0".to_string(), "A".to_string()),
            ("q_1".to_string(), "B".to_string()),
        ]);
        let out = build_control_response_ask_answer("req-m", &input, &answers);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["response"]["response"]["updatedInput"]["answers"]["q_0"], "A");
        assert_eq!(v["response"]["response"]["updatedInput"]["answers"]["q_1"], "B");
    }

    #[test]
    fn build_control_response_ask_answer_preserves_original_fields() {
        let input = serde_json::json!({"questions": [{"question": "pick?"}], "extra": 42});
        let answers = HashMap::from([("q_0".to_string(), "Red".to_string())]);
        let out = build_control_response_ask_answer("req-2", &input, &answers);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["response"]["response"]["updatedInput"]["extra"], 42);
        assert_eq!(v["response"]["response"]["updatedInput"]["questions"][0]["question"], "pick?");
    }

    #[test]
    fn build_control_response_ask_answer_ends_with_newline() {
        let input = serde_json::json!({});
        let answers = HashMap::from([("q_0".to_string(), "test".to_string())]);
        let out = build_control_response_ask_answer("req-3", &input, &answers);
        assert!(out.ends_with('\n'));
    }
}

#[cfg(test)]
mod probe_mode_tests {
    use super::*;

    #[test]
    fn probe_mode_none_is_identical_to_original() {
        let input = serde_json::json!({"command": "ls"});
        let orig = build_control_response_allow("req-1", &input);
        let probed = build_control_response_allow_probed("req-1", &input, &ProbeMode::None, "Bash");
        let v_orig: serde_json::Value = serde_json::from_str(orig.trim()).unwrap();
        let v_probed: serde_json::Value = serde_json::from_str(probed.trim()).unwrap();
        assert_eq!(v_orig["response"]["response"]["behavior"], v_probed["response"]["response"]["behavior"]);
        assert!(v_probed["response"]["response"].get("__pidory_probe").is_none());
        assert!(v_probed["response"]["response"].get("updatedPermissions").is_none());
    }

    #[test]
    fn probe_mode_bogus_adds_probe_field() {
        let input = serde_json::json!({});
        let probed = build_control_response_allow_probed("req-2", &input, &ProbeMode::Bogus, "Bash");
        let v: serde_json::Value = serde_json::from_str(probed.trim()).unwrap();
        assert_eq!(v["response"]["response"]["__pidory_probe"], true);
        assert_eq!(v["response"]["response"]["behavior"], "allow");
    }

    #[test]
    fn probe_mode_real_adds_updated_permissions_field() {
        let input = serde_json::json!({"command": "ls"});
        let probed = build_control_response_allow_probed("req-3", &input, &ProbeMode::Real, "Bash");
        let v: serde_json::Value = serde_json::from_str(probed.trim()).unwrap();
        assert!(v["response"]["response"]["updatedPermissions"].is_array());
    }

    #[test]
    fn probe_mode_from_env_defaults_to_none() {
        unsafe { std::env::remove_var("PIDORY_SPIKE_PROBE") };
        assert_eq!(ProbeMode::from_env(), ProbeMode::None);
    }

    #[test]
    fn control_response_updated_permissions_bash() {
        let input = serde_json::json!({"command": "npm test"});
        let probed = build_control_response_allow_probed("req-1", &input, &ProbeMode::Real, "Bash");
        let v: serde_json::Value = serde_json::from_str(probed.trim()).unwrap();
        let perms = &v["response"]["response"]["updatedPermissions"];
        assert!(perms.is_array());
        assert_eq!(perms[0]["type"], "addRules");
        assert_eq!(perms[0]["behavior"], "allow");
        assert_eq!(perms[0]["destination"], "session");
        assert_eq!(perms[0]["rules"][0]["toolName"], "Bash");
        assert_eq!(perms[0]["rules"][0]["ruleContent"], "npm test");
    }

    #[test]
    fn control_response_updated_permissions_webfetch_domain() {
        let input = serde_json::json!({"url": "https://example.com/foo"});
        let probed = build_control_response_allow_probed("req-2", &input, &ProbeMode::Real, "WebFetch");
        let v: serde_json::Value = serde_json::from_str(probed.trim()).unwrap();
        let perms = &v["response"]["response"]["updatedPermissions"];
        assert_eq!(perms[0]["rules"][0]["toolName"], "WebFetch");
        assert_eq!(perms[0]["rules"][0]["ruleContent"], "domain:example.com");
    }

    #[test]
    fn control_response_updated_permissions_grep_returns_none() {
        let input = serde_json::json!({"pattern": "*.rs"});
        let probed = build_control_response_allow_probed("req-3", &input, &ProbeMode::Real, "Grep");
        let v: serde_json::Value = serde_json::from_str(probed.trim()).unwrap();
        assert!(v["response"]["response"].get("updatedPermissions").is_none(),
            "Grep 은 derive_rule_content 에서 None → updatedPermissions 필드 자체가 없어야 함");
    }

    #[test]
    fn control_response_probe_none_ignores_tool_name() {
        let input = serde_json::json!({"command": "ls"});
        let probed = build_control_response_allow_probed("req-4", &input, &ProbeMode::None, "Bash");
        let v: serde_json::Value = serde_json::from_str(probed.trim()).unwrap();
        assert!(v["response"]["response"].get("updatedPermissions").is_none());
        assert!(v["response"]["response"].get("__pidory_probe").is_none());
    }
}
