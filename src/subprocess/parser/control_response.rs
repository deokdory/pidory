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
