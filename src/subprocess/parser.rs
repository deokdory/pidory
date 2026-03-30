use serde_json::Value;

#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    Thinking(String),
    ToolUse { id: String, name: String, input: Value },
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Init {
        session_id: String,
        cwd: String,
        tools: Vec<String>,
        model: String,
    },
    Assistant {
        content: Vec<ContentBlock>,
        session_id: String,
    },
    User {
        tool_results: Vec<ToolResult>,
        session_id: String,
    },
    RateLimit {
        status: String,
        resets_at: Option<u64>,
        session_id: String,
    },
    Result {
        subtype: String,
        session_id: String,
        is_error: bool,
        result: Option<String>,
        errors: Vec<String>,
        duration_ms: u64,
        total_cost_usd: f64,
        num_turns: u32,
    },
    Unknown {
        raw: Value,
    },
}

impl StreamEvent {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            StreamEvent::Init { session_id, .. } => Some(session_id),
            StreamEvent::Assistant { session_id, .. } => Some(session_id),
            StreamEvent::User { session_id, .. } => Some(session_id),
            StreamEvent::RateLimit { session_id, .. } => Some(session_id),
            StreamEvent::Result { session_id, .. } => Some(session_id),
            StreamEvent::Unknown { .. } => None,
        }
    }

    pub fn extract_text(&self) -> Option<String> {
        if let StreamEvent::Assistant { content, .. } = self {
            let texts: Vec<String> = content
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text(t) = block {
                        Some(t.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join(""))
            }
        } else {
            None
        }
    }

    pub fn is_result(&self) -> bool {
        matches!(self, StreamEvent::Result { .. })
    }

    pub fn is_error(&self) -> bool {
        if let StreamEvent::Result { is_error, .. } = self {
            *is_error
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_init_event() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123","cwd":"/tmp","tools":["Bash","Read"],"model":"claude-opus-4-6"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Init { session_id, cwd, tools, model } = event {
            assert_eq!(session_id, "abc-123");
            assert_eq!(cwd, "/tmp");
            assert_eq!(tools, vec!["Bash", "Read"]);
            assert_eq!(model, "claude-opus-4-6");
        } else {
            panic!("Expected Init event");
        }
    }

    #[test]
    fn parse_assistant_text() {
        let line = r#"{"type":"assistant","session_id":"abc","message":{"content":[{"type":"text","text":"Hello!"}]}}"#;
        let event = parse_line(line).unwrap();
        assert_eq!(event.extract_text(), Some("Hello!".to_string()));
    }

    #[test]
    fn parse_assistant_tool_use() {
        let line = r#"{"type":"assistant","session_id":"abc","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo hi"}}]}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Assistant { content, .. } = event {
            assert!(matches!(&content[0], ContentBlock::ToolUse { name, .. } if name == "Bash"));
        } else {
            panic!("Expected Assistant");
        }
    }

    #[test]
    fn parse_user_tool_result() {
        let line = r#"{"type":"user","session_id":"abc","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"hello","is_error":false}]}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::User { tool_results, .. } = event {
            assert_eq!(tool_results[0].content, "hello");
            assert!(!tool_results[0].is_error);
        } else {
            panic!("Expected User");
        }
    }

    #[test]
    fn parse_result_success() {
        let line = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"result":"done","errors":[],"duration_ms":1000,"total_cost_usd":0.05,"num_turns":1}"#;
        let event = parse_line(line).unwrap();
        assert!(event.is_result());
        assert!(!event.is_error());
        if let StreamEvent::Result { session_id, total_cost_usd, .. } = event {
            assert_eq!(session_id, "abc");
            assert!((total_cost_usd - 0.05).abs() < 0.001);
        }
    }

    #[test]
    fn parse_result_error() {
        let line = r#"{"type":"result","subtype":"error_during_execution","session_id":"xyz","is_error":true,"errors":["No conversation found"],"duration_ms":0,"total_cost_usd":0,"num_turns":0}"#;
        let event = parse_line(line).unwrap();
        assert!(event.is_error());
        if let StreamEvent::Result { errors, .. } = event {
            assert_eq!(errors[0], "No conversation found");
        }
    }

    #[test]
    fn parse_rate_limit() {
        let line = r#"{"type":"rate_limit_event","session_id":"abc","rate_limit_info":{"status":"allowed","resetsAt":12345}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::RateLimit { status, resets_at, .. } = event {
            assert_eq!(status, "allowed");
            assert_eq!(resets_at, Some(12345));
        } else {
            panic!("Expected RateLimit");
        }
    }

    #[test]
    fn parse_empty_line() {
        let event = parse_line("").unwrap();
        assert!(matches!(event, StreamEvent::Unknown { .. }));
    }

    #[test]
    fn parse_unknown_type() {
        let line = r#"{"type":"stream_event","session_id":"abc"}"#;
        let event = parse_line(line).unwrap();
        assert!(matches!(event, StreamEvent::Unknown { .. }));
    }

    #[test]
    fn parse_malformed_json() {
        let result = parse_line("not json");
        assert!(result.is_err());
    }

    #[test]
    fn session_id_extraction() {
        let line = r#"{"type":"result","subtype":"success","session_id":"my-id","is_error":false,"duration_ms":0,"total_cost_usd":0,"num_turns":0}"#;
        let event = parse_line(line).unwrap();
        assert_eq!(event.session_id(), Some("my-id"));
    }

    #[test]
    fn parse_thinking_block() {
        let line = r#"{"type":"assistant","session_id":"abc","message":{"content":[{"type":"thinking","thinking":"let me think..."}]}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Assistant { content, .. } = event {
            assert!(matches!(&content[0], ContentBlock::Thinking(t) if t == "let me think..."));
        } else {
            panic!("Expected Assistant");
        }
    }
}

pub fn parse_line(line: &str) -> Result<StreamEvent, serde_json::Error> {
    if line.trim().is_empty() {
        return Ok(StreamEvent::Unknown {
            raw: Value::Null,
        });
    }

    let v: Value = serde_json::from_str(line)?;

    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "system" => {
            let subtype = v.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
            if subtype == "init" {
                let session_id = v
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let cwd = v
                    .get("cwd")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let tools = v
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let model = v
                    .get("model")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(StreamEvent::Init {
                    session_id,
                    cwd,
                    tools,
                    model,
                })
            } else {
                Ok(StreamEvent::Unknown { raw: v })
            }
        }
        "assistant" => {
            let session_id = v
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let content_arr = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());
            let mut content = Vec::new();
            if let Some(arr) = content_arr {
                for block in arr {
                    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match block_type {
                        "text" => {
                            let text = block
                                .get("text")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            content.push(ContentBlock::Text(text));
                        }
                        "thinking" => {
                            let thinking = block
                                .get("thinking")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            content.push(ContentBlock::Thinking(thinking));
                        }
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            content.push(ContentBlock::ToolUse { id, name, input });
                        }
                        _ => {}
                    }
                }
            }
            Ok(StreamEvent::Assistant { content, session_id })
        }
        "user" => {
            let session_id = v
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let content_arr = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());
            let mut tool_results = Vec::new();
            if let Some(arr) = content_arr {
                for block in arr {
                    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if block_type == "tool_result" {
                        let tool_use_id = block
                            .get("tool_use_id")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string();
                        let content_str = block
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        let is_error = block
                            .get("is_error")
                            .and_then(|e| e.as_bool())
                            .unwrap_or(false);
                        tool_results.push(ToolResult {
                            tool_use_id,
                            content: content_str,
                            is_error,
                        });
                    }
                }
            }
            Ok(StreamEvent::User {
                tool_results,
                session_id,
            })
        }
        "rate_limit_event" => {
            let session_id = v
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let rate_limit_info = v.get("rate_limit_info");
            let status = rate_limit_info
                .and_then(|r| r.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let resets_at = rate_limit_info
                .and_then(|r| r.get("resetsAt"))
                .and_then(|r| r.as_u64());
            Ok(StreamEvent::RateLimit {
                status,
                resets_at,
                session_id,
            })
        }
        "result" => {
            let session_id = v
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let subtype = v
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let is_error = v
                .get("is_error")
                .and_then(|e| e.as_bool())
                .unwrap_or(false);
            let result = v
                .get("result")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());
            let errors = v
                .get("errors")
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| e.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let duration_ms = v
                .get("duration_ms")
                .and_then(|d| d.as_u64())
                .unwrap_or(0);
            let total_cost_usd = v
                .get("total_cost_usd")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0);
            let num_turns = v
                .get("num_turns")
                .and_then(|n| n.as_u64())
                .unwrap_or(0) as u32;
            Ok(StreamEvent::Result {
                subtype,
                session_id,
                is_error,
                result,
                errors,
                duration_ms,
                total_cost_usd,
                num_turns,
            })
        }
        _ => Ok(StreamEvent::Unknown { raw: v }),
    }
}
