use serde_json::Value;

use super::types::{ContentBlock, StreamEvent, ToolResult};

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
                let skills = v
                    .get("skills")
                    .and_then(|s| s.as_array())
                    .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                Ok(StreamEvent::Init {
                    session_id,
                    cwd,
                    tools,
                    model,
                    skills,
                })
            } else if subtype == "task_started" {
                let task_id = v
                    .get("task_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_use_id = v
                    .get("tool_use_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = v
                    .get("description")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let task_type = v
                    .get("task_type")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let session_id = v
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(StreamEvent::TaskStarted {
                    task_id,
                    tool_use_id,
                    description,
                    task_type,
                    session_id,
                })
            } else if subtype == "task_progress" {
                let task_id = v
                    .get("task_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_use_id = v
                    .get("tool_use_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = v
                    .get("description")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let last_tool_name = v
                    .get("last_tool_name")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let session_id = v
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(StreamEvent::TaskProgress {
                    task_id,
                    tool_use_id,
                    description,
                    last_tool_name,
                    session_id,
                })
            } else if subtype == "task_notification" {
                let task_id = v
                    .get("task_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_use_id = v
                    .get("tool_use_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let status = v
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let summary = v
                    .get("summary")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let output_file = v
                    .get("output_file")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let session_id = v
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(StreamEvent::TaskNotification {
                    task_id,
                    tool_use_id,
                    status,
                    summary,
                    output_file,
                    session_id,
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

            let is_replay = v.get("isReplay").and_then(|r| r.as_bool()).unwrap_or(false);

            if is_replay {
                let text = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .and_then(|arr| {
                        arr.iter().find_map(|b| {
                            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or_default();
                let timestamp = v
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string());
                Ok(StreamEvent::UserReplay {
                    content: text,
                    session_id,
                    timestamp,
                })
            } else {
                let content_arr = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array());
                let mut tool_results = Vec::new();
                if let Some(arr) = content_arr {
                    for block in arr {
                        let block_type =
                            block.get("type").and_then(|t| t.as_str()).unwrap_or("");
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
            let rate_limit_type = rate_limit_info
                .and_then(|r| r.get("rateLimitType"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            let utilization = rate_limit_info
                .and_then(|r| r.get("utilization"))
                .and_then(|u| u.as_f64());
            let is_using_overage = rate_limit_info
                .and_then(|r| r.get("isUsingOverage"))
                .and_then(|b| b.as_bool());
            Ok(StreamEvent::RateLimit {
                status,
                resets_at,
                session_id,
                rate_limit_type,
                utilization,
                is_using_overage,
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
            let usage = v.get("usage");
            let input_tokens = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output_tokens = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let main_model = v.get("modelUsage")
                .and_then(|m| m.as_object())
                .and_then(|obj| {
                    obj.values().max_by_key(|model| {
                        model.get("contextWindow").and_then(|v| v.as_u64()).unwrap_or(0)
                    })
                });
            let context_window = main_model
                .and_then(|model| model.get("contextWindow"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_creation_input_tokens = usage
                .and_then(|u| u.get("cache_creation_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_read_input_tokens = usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total_input_tokens = input_tokens
                .saturating_add(cache_creation_input_tokens)
                .saturating_add(cache_read_input_tokens);
            Ok(StreamEvent::Result {
                subtype,
                session_id,
                is_error,
                result,
                errors,
                duration_ms,
                total_cost_usd,
                num_turns,
                input_tokens,
                output_tokens,
                context_window,
                total_input_tokens,
            })
        }
        "control_request" => {
            let request_id = v
                .get("request_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let request = v.get("request");
            let tool_name = request
                .and_then(|r| r.get("tool_name"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let tool_use_id = request
                .and_then(|r| r.get("tool_use_id"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let input = request
                .and_then(|r| r.get("input"))
                .cloned()
                .unwrap_or(Value::Null);
            let decision_reason = request
                .and_then(|r| r.get("decision_reason"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            Ok(StreamEvent::ControlRequest {
                request_id,
                tool_name,
                tool_use_id,
                input,
                decision_reason,
            })
        }
        _ => Ok(StreamEvent::Unknown { raw: v }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_init_event() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123","cwd":"/tmp","tools":["Bash","Read"],"model":"claude-opus-4-6","skills":["craft","verify"]}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Init { session_id, cwd, tools, model, skills } = event {
            assert_eq!(session_id, "abc-123");
            assert_eq!(cwd, "/tmp");
            assert_eq!(tools, vec!["Bash", "Read"]);
            assert_eq!(model, "claude-opus-4-6");
            assert_eq!(skills, vec!["craft", "verify"]);
        } else {
            panic!("Expected Init event");
        }
    }

    #[test]
    fn parse_init_with_skills() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc","cwd":"/tmp","tools":["Bash"],"model":"claude-opus-4-6","skills":["craft","verify","build"]}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Init { skills, .. } = event {
            assert_eq!(skills, vec!["craft", "verify", "build"]);
        } else {
            panic!("Expected Init");
        }
    }

    #[test]
    fn parse_init_without_skills() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc","cwd":"/tmp","tools":["Bash"],"model":"opus"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Init { skills, .. } = event {
            assert!(skills.is_empty());
        } else {
            panic!("Expected Init");
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
        if let StreamEvent::RateLimit { status, resets_at, rate_limit_type, utilization, is_using_overage, .. } = event {
            assert_eq!(status, "allowed");
            assert_eq!(resets_at, Some(12345));
            assert_eq!(rate_limit_type, None);
            assert_eq!(utilization, None);
            assert_eq!(is_using_overage, None);
        } else {
            panic!("Expected RateLimit");
        }
    }

    #[test]
    fn parse_rate_limit_with_utilization() {
        let line = r#"{"type":"rate_limit_event","session_id":"abc","rate_limit_info":{"status":"allowed_warning","resetsAt":1776042000,"rateLimitType":"seven_day","utilization":0.57,"isUsingOverage":false}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::RateLimit { status, resets_at, rate_limit_type, utilization, is_using_overage, .. } = event {
            assert_eq!(status, "allowed_warning");
            assert_eq!(resets_at, Some(1776042000));
            assert_eq!(rate_limit_type, Some("seven_day".to_string()));
            assert!((utilization.unwrap() - 0.57).abs() < 0.001);
            assert_eq!(is_using_overage, Some(false));
        } else {
            panic!("Expected RateLimit");
        }
    }

    #[test]
    fn parse_rate_limit_five_hour() {
        let line = r#"{"type":"rate_limit_event","session_id":"abc","rate_limit_info":{"status":"allowed","resetsAt":12345,"rateLimitType":"five_hour","utilization":0.24,"isUsingOverage":false}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::RateLimit { rate_limit_type, utilization, .. } = event {
            assert_eq!(rate_limit_type, Some("five_hour".to_string()));
            assert!((utilization.unwrap() - 0.24).abs() < 0.001);
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
    fn parse_user_replay() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"say hello"}]},"isReplay":true,"timestamp":"2026-03-31T00:00:00Z","session_id":"abc"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::UserReplay { content, session_id, timestamp } = event {
            assert_eq!(content, "say hello");
            assert_eq!(session_id, "abc");
            assert_eq!(timestamp, Some("2026-03-31T00:00:00Z".to_string()));
        } else {
            panic!("Expected UserReplay, got {:?}", event);
        }
    }

    #[test]
    fn parse_user_replay_vs_tool_result() {
        // isReplay가 없는 기존 tool_result는 User variant로
        let line = r#"{"type":"user","session_id":"abc","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"hello","is_error":false}]}}"#;
        let event = parse_line(line).unwrap();
        assert!(matches!(event, StreamEvent::User { .. }));
    }

    #[test]
    fn user_replay_session_id() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"isReplay":true,"session_id":"my-session"}"#;
        let event = parse_line(line).unwrap();
        assert_eq!(event.session_id(), Some("my-session"));
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

    #[test]
    fn parse_control_request() {
        let line = r#"{"type":"control_request","request_id":"e5c3058b-6794-4a0d-b445-7729855cb810","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"/tmp/test.txt","content":"hello"},"permission_suggestions":[],"decision_reason":"Path is outside allowed working directories","tool_use_id":"toolu_01BKN27SrcApvHEMYi7A1ik4"}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::ControlRequest { request_id, tool_name, tool_use_id, input, decision_reason } = event {
            assert_eq!(request_id, "e5c3058b-6794-4a0d-b445-7729855cb810");
            assert_eq!(tool_name, "Write");
            assert_eq!(tool_use_id, "toolu_01BKN27SrcApvHEMYi7A1ik4");
            assert_eq!(input["file_path"], "/tmp/test.txt");
            assert_eq!(decision_reason, Some("Path is outside allowed working directories".to_string()));
        } else {
            panic!("Expected ControlRequest, got {:?}", event);
        }
    }

    #[test]
    fn parse_control_request_is_control_request() {
        let line = r#"{"type":"control_request","request_id":"abc","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"t1"}}"#;
        let event = parse_line(line).unwrap();
        assert!(event.is_control_request());
        assert!(!event.is_result());
        assert_eq!(event.session_id(), None);
    }

    #[test]
    fn parse_control_request_no_decision_reason() {
        let line = r#"{"type":"control_request","request_id":"abc","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"file_path":"/tmp/f"},"tool_use_id":"t1"}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::ControlRequest { decision_reason, .. } = event {
            assert_eq!(decision_reason, None);
        } else {
            panic!("Expected ControlRequest");
        }
    }

    #[test]
    fn parse_task_started() {
        let line = r#"{"type":"system","subtype":"task_started","task_id":"buyj7z5o7","tool_use_id":"toolu_012daJxCZsawPJKJYF6WxmtC","description":"Sleep 3 seconds then print bg_task_done","task_type":"local_bash","session_id":"40746b0a-41c2-4f62-9ea6-d683612ad9ae"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::TaskStarted { task_id, tool_use_id, description, task_type, session_id } = event {
            assert_eq!(task_id, "buyj7z5o7");
            assert_eq!(tool_use_id, "toolu_012daJxCZsawPJKJYF6WxmtC");
            assert_eq!(description, "Sleep 3 seconds then print bg_task_done");
            assert_eq!(task_type, "local_bash");
            assert_eq!(session_id, "40746b0a-41c2-4f62-9ea6-d683612ad9ae");
        } else {
            panic!("Expected TaskStarted, got {:?}", event);
        }
    }

    #[test]
    fn parse_task_started_agent_type() {
        let line = r#"{"type":"system","subtype":"task_started","task_id":"a7ca5a342d867c971","tool_use_id":"toolu_01CWFNoUWUwyfyMqVJ42F96Z","description":"Read /etc/hostname content","task_type":"local_agent","session_id":"a352a7c9-4254-465e-b444-b804c6099892"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::TaskStarted { task_type, .. } = event {
            assert_eq!(task_type, "local_agent");
        } else {
            panic!("Expected TaskStarted");
        }
    }

    #[test]
    fn parse_task_started_session_id() {
        let line = r#"{"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"tu1","description":"d","task_type":"local_bash","session_id":"sess-abc"}"#;
        let event = parse_line(line).unwrap();
        assert_eq!(event.session_id(), Some("sess-abc"));
    }

    #[test]
    fn parse_task_started_missing_fields() {
        let line = r#"{"type":"system","subtype":"task_started","task_id":"t1","session_id":"s1"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::TaskStarted { tool_use_id, description, task_type, .. } = event {
            assert_eq!(tool_use_id, "");
            assert_eq!(description, "");
            assert_eq!(task_type, "");
        } else {
            panic!("Expected TaskStarted");
        }
    }

    #[test]
    fn parse_task_progress() {
        let line = r#"{"type":"system","subtype":"task_progress","task_id":"a7ca5a342d867c971","tool_use_id":"toolu_01CWFNoUWUwyfyMqVJ42F96Z","description":"Reading /etc/hostname","usage":{"total_tokens":10950},"last_tool_name":"Read","session_id":"a352a7c9-4254-465e-b444-b804c6099892"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::TaskProgress { task_id, tool_use_id, description, last_tool_name, session_id } = event {
            assert_eq!(task_id, "a7ca5a342d867c971");
            assert_eq!(tool_use_id, "toolu_01CWFNoUWUwyfyMqVJ42F96Z");
            assert_eq!(description, "Reading /etc/hostname");
            assert_eq!(last_tool_name, Some("Read".to_string()));
            assert_eq!(session_id, "a352a7c9-4254-465e-b444-b804c6099892");
        } else {
            panic!("Expected TaskProgress, got {:?}", event);
        }
    }

    #[test]
    fn parse_task_progress_no_last_tool_name() {
        let line = r#"{"type":"system","subtype":"task_progress","task_id":"t1","tool_use_id":"tu1","description":"doing stuff","session_id":"s1"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::TaskProgress { last_tool_name, .. } = event {
            assert_eq!(last_tool_name, None);
        } else {
            panic!("Expected TaskProgress");
        }
    }

    #[test]
    fn parse_task_progress_session_id() {
        let line = r#"{"type":"system","subtype":"task_progress","task_id":"t1","tool_use_id":"tu1","description":"d","session_id":"my-session"}"#;
        let event = parse_line(line).unwrap();
        assert_eq!(event.session_id(), Some("my-session"));
    }

    #[test]
    fn parse_task_notification_completed() {
        let line = r#"{"type":"system","subtype":"task_notification","task_id":"buyj7z5o7","tool_use_id":"toolu_012daJxCZsawPJKJYF6WxmtC","status":"completed","output_file":"/tmp/claude-1000/tasks/buyj7z5o7.output","summary":"Background command completed (exit code 0)","session_id":"40746b0a-41c2-4f62-9ea6-d683612ad9ae"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::TaskNotification { task_id, tool_use_id, status, summary, output_file, session_id } = event {
            assert_eq!(task_id, "buyj7z5o7");
            assert_eq!(tool_use_id, "toolu_012daJxCZsawPJKJYF6WxmtC");
            assert_eq!(status, "completed");
            assert_eq!(summary, "Background command completed (exit code 0)");
            assert_eq!(output_file, Some("/tmp/claude-1000/tasks/buyj7z5o7.output".to_string()));
            assert_eq!(session_id, "40746b0a-41c2-4f62-9ea6-d683612ad9ae");
        } else {
            panic!("Expected TaskNotification, got {:?}", event);
        }
    }

    #[test]
    fn parse_task_notification_failed_no_output_file() {
        let line = r#"{"type":"system","subtype":"task_notification","task_id":"bf2vp1kx2","status":"failed","summary":"Background command failed with exit code 1","session_id":"sess-xyz"}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::TaskNotification { task_id, tool_use_id, status, output_file, .. } = event {
            assert_eq!(task_id, "bf2vp1kx2");
            assert_eq!(tool_use_id, "");
            assert_eq!(status, "failed");
            assert_eq!(output_file, None);
        } else {
            panic!("Expected TaskNotification");
        }
    }

    #[test]
    fn parse_task_notification_session_id() {
        let line = r#"{"type":"system","subtype":"task_notification","task_id":"t1","status":"completed","summary":"done","session_id":"sid-123"}"#;
        let event = parse_line(line).unwrap();
        assert_eq!(event.session_id(), Some("sid-123"));
    }

    #[test]
    fn is_task_notification_true() {
        let line = r#"{"type":"system","subtype":"task_notification","task_id":"t1","status":"completed","summary":"done","session_id":"s1"}"#;
        let event = parse_line(line).unwrap();
        assert!(event.is_task_notification());
        assert!(!event.is_result());
    }

    #[test]
    fn is_task_notification_false_for_others() {
        let line = r#"{"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"tu1","description":"d","task_type":"local_bash","session_id":"s1"}"#;
        let event = parse_line(line).unwrap();
        assert!(!event.is_task_notification());

        let line2 = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"duration_ms":0,"total_cost_usd":0,"num_turns":0}"#;
        let event2 = parse_line(line2).unwrap();
        assert!(!event2.is_task_notification());
    }

    #[test]
    fn parse_result_with_model_usage() {
        let line = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"duration_ms":1000,"total_cost_usd":0.16,"num_turns":1,"usage":{"input_tokens":3,"cache_creation_input_tokens":26147,"cache_read_input_tokens":0,"output_tokens":4},"modelUsage":{"claude-opus-4-6[1m]":{"inputTokens":3,"cacheCreationInputTokens":26147,"cacheReadInputTokens":0,"outputTokens":4,"contextWindow":1000000,"maxOutputTokens":64000}}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Result { context_window, input_tokens, total_input_tokens, .. } = event {
            assert_eq!(context_window, 1000000);
            assert_eq!(input_tokens, 3); // top-level usage.input_tokens
            assert_eq!(total_input_tokens, 26150); // 3 + 26147 + 0 (top-level usage)
        } else {
            panic!("expected Result");
        }
    }

    #[test]
    fn parse_result_with_multiple_models() {
        // 200K + 1M models: should pick 1M model for context_window, total_input_tokens from top-level usage
        let line = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"duration_ms":5000,"total_cost_usd":1.0,"num_turns":3,"usage":{"input_tokens":17,"cache_creation_input_tokens":263893,"cache_read_input_tokens":1667176,"output_tokens":5067},"modelUsage":{"claude-opus-4-6":{"inputTokens":9,"cacheCreationInputTokens":257281,"cacheReadInputTokens":819053,"contextWindow":200000,"outputTokens":2202},"claude-opus-4-6[1m]":{"inputTokens":8,"cacheCreationInputTokens":6612,"cacheReadInputTokens":848123,"contextWindow":1000000,"outputTokens":2865}}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Result { context_window, input_tokens, total_input_tokens, .. } = event {
            assert_eq!(context_window, 1000000); // picks the 1M model
            assert_eq!(input_tokens, 17); // top-level usage.input_tokens unchanged
            assert_eq!(total_input_tokens, 1931086); // top-level usage: 17 + 263893 + 1667176
        } else {
            panic!("expected Result");
        }
    }

    #[test]
    fn parse_result_model_without_cache_fields() {
        // modelUsage entry has no cacheCreationInputTokens / cacheReadInputTokens
        let line = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"duration_ms":500,"total_cost_usd":0.01,"num_turns":1,"usage":{"input_tokens":10,"output_tokens":20},"modelUsage":{"claude-3-5-haiku":{"inputTokens":10,"outputTokens":20,"contextWindow":200000}}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Result { context_window, total_input_tokens, .. } = event {
            assert_eq!(context_window, 200000);
            assert_eq!(total_input_tokens, 10); // only inputTokens, no cache fields
        } else {
            panic!("expected Result");
        }
    }

    #[test]
    fn parse_result_without_cache_fields() {
        let line = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"duration_ms":500,"total_cost_usd":0.05,"num_turns":1,"usage":{"input_tokens":500,"output_tokens":100}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Result { input_tokens, total_input_tokens, .. } = event {
            assert_eq!(input_tokens, 500);
            assert_eq!(total_input_tokens, 500); // no cache fields → total == input
        } else {
            panic!("expected Result");
        }
    }

    #[test]
    fn parse_result_empty_model_usage() {
        let line = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"duration_ms":0,"total_cost_usd":0,"num_turns":0,"modelUsage":{}}"#;
        let event = parse_line(line).unwrap();
        if let StreamEvent::Result { context_window, .. } = event {
            assert_eq!(context_window, 0);
        } else {
            panic!("expected Result");
        }
    }
}
