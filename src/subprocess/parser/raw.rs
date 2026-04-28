//! Raw wire format types for Claude CLI stream-json output.
//!
//! Structs are deserialized directly from JSON lines with lenient fallbacks
//! matching the silent-default behavior of the original `parse_line`.
//! Conversion to `StreamEvent` happens in `From<RawStreamEvent>` (task 2.1).

use std::collections::HashMap;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub(super) fn deserialize_string_lenient<'de, D>(deserializer: D) -> Result<String, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(s) => s,
        _ => String::new(),
    })
}

pub(super) fn deserialize_u64_lenient<'de, D>(deserializer: D) -> Result<u64, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    })
}

pub(super) fn deserialize_u32_lenient<'de, D>(deserializer: D) -> Result<u32, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Number(n) => n.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        _ => 0,
    })
}

pub(super) fn deserialize_f64_lenient<'de, D>(deserializer: D) -> Result<f64, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    })
}

pub(super) fn deserialize_bool_lenient<'de, D>(deserializer: D) -> Result<bool, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Bool(b) => b,
        _ => false,
    })
}

pub(super) fn deserialize_string_vec_lenient<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => vec![],
    })
}

// Optional lenient deserializers — wrong-shape JSON returns None instead of error.
// Mirrors original parse_line's `.and_then(|x| x.as_<t>())` silent-fallback pattern.

pub(super) fn deserialize_optional_u64_lenient<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Number(n) => n.as_u64(),
        _ => None,
    })
}

pub(super) fn deserialize_optional_f64_lenient<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Number(n) => n.as_f64(),
        _ => None,
    })
}

pub(super) fn deserialize_optional_bool_lenient<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Bool(b) => Some(b),
        _ => None,
    })
}

pub(super) fn deserialize_optional_string_lenient<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where D: Deserializer<'de> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(s) => Some(s),
        _ => None,
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum RawStreamEvent {
    System(RawSystemEvent),
    Assistant(RawAssistant),
    User(RawUser),
    #[serde(rename = "rate_limit_event")]
    RateLimitEvent(RawRateLimit),
    Result(RawResult),
    ControlRequest(RawControlRequest),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub(super) enum RawSystemEvent {
    Init(RawInit),
    TaskStarted(RawTaskStarted),
    TaskProgress(RawTaskProgress),
    TaskNotification(RawTaskNotification),
    CompactBoundary(RawCompactBoundary),
}

#[derive(Debug, Deserialize)]
pub(super) struct RawInit {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) cwd: String,
    #[serde(default, deserialize_with = "deserialize_string_vec_lenient")]
    pub(super) tools: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) model: String,
    #[serde(default, deserialize_with = "deserialize_string_vec_lenient")]
    pub(super) skills: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawAssistant {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
    // baseline parse_line treated missing `message` as empty content vec;
    // keep that behaviour by defaulting the whole sub-struct.
    #[serde(default)]
    pub(super) message: RawAssistantMessage,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawAssistantMessage {
    #[serde(default)]
    pub(super) content: Vec<RawContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum RawContentBlock {
    Text {
        #[serde(default, deserialize_with = "deserialize_string_lenient")]
        text: String,
    },
    Thinking {
        #[serde(default, deserialize_with = "deserialize_string_lenient")]
        thinking: String,
    },
    ToolUse {
        #[serde(default, deserialize_with = "deserialize_string_lenient")]
        id: String,
        #[serde(default, deserialize_with = "deserialize_string_lenient")]
        name: String,
        #[serde(default)]
        input: Value,
    },
    // Catch-all — baseline `_ => {}` silently skipped unknown content block types.
    // Without this variant a single unknown block breaks the whole Assistant event.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawUser {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
    // missing `message` → empty content vec (baseline silent-default).
    #[serde(default)]
    pub(super) message: RawUserMessage,
    #[serde(default, rename = "isReplay", deserialize_with = "deserialize_bool_lenient")]
    pub(super) is_replay: bool,
    #[serde(default, deserialize_with = "deserialize_optional_string_lenient")]
    pub(super) timestamp: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawUserMessage {
    #[serde(default)]
    pub(super) content: Vec<RawUserContent>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum RawUserContent {
    ToolResult {
        #[serde(default, deserialize_with = "deserialize_string_lenient")]
        tool_use_id: String,
        #[serde(default, deserialize_with = "deserialize_string_lenient")]
        content: String,
        #[serde(default, deserialize_with = "deserialize_bool_lenient")]
        is_error: bool,
    },
    Text {
        #[serde(default, deserialize_with = "deserialize_string_lenient")]
        text: String,
    },
    // Catch-all — baseline silently skipped unknown user content block types.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawRateLimit {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) rate_limit_info: Option<RawRateLimitInfo>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawRateLimitInfo {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) status: String,
    // Optional fields with lenient deserializers — baseline used `.and_then(|x| x.as_u64())`
    // so wrong-shape JSON (e.g. resetsAt as string) silently became None.
    #[serde(rename = "resetsAt", default, deserialize_with = "deserialize_optional_u64_lenient")]
    pub(super) resets_at: Option<u64>,
    #[serde(rename = "rateLimitType", default, deserialize_with = "deserialize_optional_string_lenient")]
    pub(super) rate_limit_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_f64_lenient")]
    pub(super) utilization: Option<f64>,
    #[serde(rename = "isUsingOverage", default, deserialize_with = "deserialize_optional_bool_lenient")]
    pub(super) is_using_overage: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawResult {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) subtype: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
    #[serde(default, deserialize_with = "deserialize_bool_lenient")]
    pub(super) is_error: bool,
    #[serde(default)]
    pub(super) result: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec_lenient")]
    pub(super) errors: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_u64_lenient")]
    pub(super) duration_ms: u64,
    #[serde(default, deserialize_with = "deserialize_f64_lenient")]
    pub(super) total_cost_usd: f64,
    #[serde(default, deserialize_with = "deserialize_u32_lenient")]
    pub(super) num_turns: u32,
    #[serde(default)]
    pub(super) usage: Option<RawResultUsage>,
    #[serde(rename = "modelUsage", default)]
    pub(super) model_usage: Option<HashMap<String, RawModelUsage>>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawResultUsage {
    #[serde(default, deserialize_with = "deserialize_u64_lenient")]
    pub(super) input_tokens: u64,
    #[serde(default, deserialize_with = "deserialize_u64_lenient")]
    pub(super) output_tokens: u64,
    #[serde(rename = "cache_creation_input_tokens", default, deserialize_with = "deserialize_u64_lenient")]
    pub(super) cache_creation_input_tokens: u64,
    #[serde(rename = "cache_read_input_tokens", default, deserialize_with = "deserialize_u64_lenient")]
    pub(super) cache_read_input_tokens: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawModelUsage {
    #[serde(rename = "contextWindow", default, deserialize_with = "deserialize_u64_lenient")]
    pub(super) context_window: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawControlRequest {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) request_id: String,
    // missing `request` → empty inner (baseline `let request = v.get("request"); request.and_then(...)`).
    #[serde(default)]
    pub(super) request: RawControlRequestInner,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RawControlRequestInner {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) tool_name: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) tool_use_id: String,
    #[serde(default)]
    pub(super) input: Value,
    #[serde(default, deserialize_with = "deserialize_optional_string_lenient")]
    pub(super) decision_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawTaskStarted {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) task_id: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) tool_use_id: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) description: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) task_type: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawTaskProgress {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) task_id: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) tool_use_id: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) description: String,
    #[serde(default)]
    pub(super) last_tool_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawTaskNotification {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) task_id: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) tool_use_id: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) status: String,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) summary: String,
    #[serde(default)]
    pub(super) output_file: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawCompactBoundary {
    #[serde(default, deserialize_with = "deserialize_string_lenient")]
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) compact_metadata: Option<RawCompactMetadata>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawCompactMetadata {
    #[serde(default)]
    pub(super) pre_tokens: Option<u64>,
    #[serde(default)]
    pub(super) trigger: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::IntoDeserializer;

    fn get(json: &str, key: &str) -> Value {
        serde_json::from_str::<Value>(json).unwrap().get(key).unwrap().clone()
    }

    #[test]
    fn string_lenient_normal() {
        let s: String = deserialize_string_lenient(get(r#"{"s":"hi"}"#, "s").into_deserializer()).unwrap();
        assert_eq!(s, "hi");
    }
    #[test]
    fn string_lenient_wrong_type() {
        let s: String = deserialize_string_lenient(get(r#"{"s":42}"#, "s").into_deserializer()).unwrap();
        assert_eq!(s, "");
    }
    #[test]
    fn u64_lenient_normal() {
        let n: u64 = deserialize_u64_lenient(get(r#"{"n":99}"#, "n").into_deserializer()).unwrap();
        assert_eq!(n, 99);
    }
    #[test]
    fn u64_lenient_wrong_type() {
        let n: u64 = deserialize_u64_lenient(get(r#"{"n":"x"}"#, "n").into_deserializer()).unwrap();
        assert_eq!(n, 0);
    }
    #[test]
    fn u32_lenient_normal() {
        let n: u32 = deserialize_u32_lenient(get(r#"{"n":7}"#, "n").into_deserializer()).unwrap();
        assert_eq!(n, 7);
    }
    #[test]
    fn u32_lenient_wrong_type() {
        let n: u32 = deserialize_u32_lenient(get(r#"{"n":true}"#, "n").into_deserializer()).unwrap();
        assert_eq!(n, 0);
    }
    #[test]
    fn f64_lenient_normal() {
        let f: f64 = deserialize_f64_lenient(get(r#"{"f":2.5}"#, "f").into_deserializer()).unwrap();
        assert!((f - 2.5).abs() < 1e-9);
    }
    #[test]
    fn f64_lenient_wrong_type() {
        let f: f64 = deserialize_f64_lenient(get(r#"{"f":"no"}"#, "f").into_deserializer()).unwrap();
        assert_eq!(f, 0.0);
    }
    #[test]
    fn bool_lenient_normal() {
        let b: bool = deserialize_bool_lenient(get(r#"{"b":true}"#, "b").into_deserializer()).unwrap();
        assert!(b);
    }
    #[test]
    fn bool_lenient_wrong_type() {
        let b: bool = deserialize_bool_lenient(get(r#"{"b":1}"#, "b").into_deserializer()).unwrap();
        assert!(!b);
    }
    #[test]
    fn string_vec_lenient_normal() {
        let arr: Vec<String> = deserialize_string_vec_lenient(get(r#"{"a":["x","y"]}"#, "a").into_deserializer()).unwrap();
        assert_eq!(arr, vec!["x", "y"]);
    }
    #[test]
    fn string_vec_lenient_wrong_type() {
        let arr: Vec<String> = deserialize_string_vec_lenient(get(r#"{"a":{}}"#, "a").into_deserializer()).unwrap();
        assert!(arr.is_empty());
    }
    #[test]
    fn raw_stream_event_init_roundtrip() {
        let json = r#"{"type":"system","subtype":"init","session_id":"abc","cwd":"/tmp","tools":["Bash"],"model":"claude-3","skills":[]}"#;
        let ev: RawStreamEvent = serde_json::from_str(json).unwrap();
        let RawStreamEvent::System(RawSystemEvent::Init(init)) = ev else { panic!("expected Init") };
        assert_eq!(init.session_id, "abc");
        assert_eq!(init.tools, vec!["Bash"]);
    }
}
