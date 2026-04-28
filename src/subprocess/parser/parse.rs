use serde_json::Value;

use super::raw::{
    RawCompactBoundary, RawContentBlock, RawControlRequest, RawInit, RawRateLimit,
    RawResult, RawStreamEvent, RawSystemEvent, RawTaskNotification, RawTaskProgress,
    RawTaskStarted, RawUser, RawUserContent,
};
use super::types::{ContentBlock, StreamEvent, ToolResult};

impl From<RawContentBlock> for ContentBlock {
    fn from(raw: RawContentBlock) -> Self {
        match raw {
            RawContentBlock::Text { text } => ContentBlock::Text(text),
            RawContentBlock::Thinking { thinking } => ContentBlock::Thinking(thinking),
            RawContentBlock::ToolUse { id, name, input } => ContentBlock::ToolUse { id, name, input },
        }
    }
}

/// Extract UserReplay text: first Text variant in message.content.
/// Matches parse_line behaviour (find_map → first Text, not concat).
fn extract_user_replay_text(content: &[RawUserContent]) -> String {
    content
        .iter()
        .find_map(|c| match c {
            RawUserContent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

impl From<RawInit> for StreamEvent {
    fn from(init: RawInit) -> Self {
        StreamEvent::Init {
            session_id: init.session_id,
            cwd: init.cwd,
            tools: init.tools,
            model: init.model,
            skills: init.skills,
        }
    }
}

impl From<RawTaskStarted> for StreamEvent {
    fn from(t: RawTaskStarted) -> Self {
        StreamEvent::TaskStarted {
            task_id: t.task_id,
            tool_use_id: t.tool_use_id,
            description: t.description,
            task_type: t.task_type,
            session_id: t.session_id,
        }
    }
}

impl From<RawTaskProgress> for StreamEvent {
    fn from(t: RawTaskProgress) -> Self {
        StreamEvent::TaskProgress {
            task_id: t.task_id,
            tool_use_id: t.tool_use_id,
            description: t.description,
            last_tool_name: t.last_tool_name,
            session_id: t.session_id,
        }
    }
}

impl From<RawTaskNotification> for StreamEvent {
    fn from(t: RawTaskNotification) -> Self {
        StreamEvent::TaskNotification {
            task_id: t.task_id,
            tool_use_id: t.tool_use_id,
            status: t.status,
            summary: t.summary,
            output_file: t.output_file,
            session_id: t.session_id,
        }
    }
}

impl From<RawCompactBoundary> for StreamEvent {
    fn from(cb: RawCompactBoundary) -> Self {
        let (pre_tokens, trigger) = match cb.compact_metadata {
            Some(m) => (m.pre_tokens, m.trigger),
            None => (None, None),
        };
        StreamEvent::CompactBoundary {
            pre_tokens,
            trigger,
            session_id: cb.session_id,
        }
    }
}

impl From<RawSystemEvent> for StreamEvent {
    fn from(sys: RawSystemEvent) -> Self {
        match sys {
            RawSystemEvent::Init(init) => StreamEvent::from(init),
            RawSystemEvent::TaskStarted(t) => StreamEvent::from(t),
            RawSystemEvent::TaskProgress(t) => StreamEvent::from(t),
            RawSystemEvent::TaskNotification(t) => StreamEvent::from(t),
            RawSystemEvent::CompactBoundary(cb) => StreamEvent::from(cb),
        }
    }
}

impl From<RawRateLimit> for StreamEvent {
    fn from(r: RawRateLimit) -> Self {
        let info = r.rate_limit_info.unwrap_or_default();
        StreamEvent::RateLimit {
            status: info.status,
            resets_at: info.resets_at,
            session_id: r.session_id,
            rate_limit_type: info.rate_limit_type,
            utilization: info.utilization,
            is_using_overage: info.is_using_overage,
        }
    }
}

impl From<RawResult> for StreamEvent {
    fn from(res: RawResult) -> Self {
        let usage = res.usage.unwrap_or_default();
        let total_input_tokens = usage
            .input_tokens
            .saturating_add(usage.cache_creation_input_tokens)
            .saturating_add(usage.cache_read_input_tokens);
        let context_window = res
            .model_usage
            .as_ref()
            .and_then(|m| m.values().map(|mu| mu.context_window).max())
            .unwrap_or(0);
        StreamEvent::Result {
            subtype: res.subtype,
            session_id: res.session_id,
            is_error: res.is_error,
            result: res.result,
            errors: res.errors,
            duration_ms: res.duration_ms,
            total_cost_usd: res.total_cost_usd,
            num_turns: res.num_turns,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            context_window,
            total_input_tokens,
        }
    }
}

impl From<RawControlRequest> for StreamEvent {
    fn from(cr: RawControlRequest) -> Self {
        StreamEvent::ControlRequest {
            request_id: cr.request_id,
            tool_name: cr.request.tool_name,
            tool_use_id: cr.request.tool_use_id,
            input: cr.request.input,
            decision_reason: cr.request.decision_reason,
        }
    }
}

impl From<RawUser> for StreamEvent {
    fn from(u: RawUser) -> Self {
        if u.is_replay {
            let content = extract_user_replay_text(&u.message.content);
            StreamEvent::UserReplay {
                content,
                session_id: u.session_id,
                timestamp: u.timestamp,
            }
        } else {
            let tool_results = u
                .message
                .content
                .into_iter()
                .filter_map(|c| match c {
                    RawUserContent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => Some(ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    }),
                    RawUserContent::Text { .. } => None,
                })
                .collect();
            StreamEvent::User {
                tool_results,
                session_id: u.session_id,
            }
        }
    }
}

impl From<RawStreamEvent> for StreamEvent {
    fn from(raw: RawStreamEvent) -> Self {
        match raw {
            RawStreamEvent::System(sys) => StreamEvent::from(sys),
            RawStreamEvent::Assistant(a) => {
                let content = a
                    .message
                    .content
                    .into_iter()
                    .map(ContentBlock::from)
                    .collect();
                StreamEvent::Assistant {
                    content,
                    session_id: a.session_id,
                }
            }
            RawStreamEvent::User(u) => StreamEvent::from(u),
            RawStreamEvent::RateLimitEvent(r) => StreamEvent::from(r),
            RawStreamEvent::Result(res) => StreamEvent::from(res),
            RawStreamEvent::ControlRequest(cr) => StreamEvent::from(cr),
        }
    }
}

// ---------------------------------------------------------------------------

pub fn parse_line(line: &str) -> Result<StreamEvent, serde_json::Error> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(StreamEvent::Unknown { raw: Value::Null });
    }
    match serde_json::from_str::<RawStreamEvent>(trimmed) {
        Ok(raw) => Ok(StreamEvent::from(raw)),
        Err(_) => {
            let v: Value = serde_json::from_str(trimmed)?;
            Ok(StreamEvent::Unknown { raw: v })
        }
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
    fn parse_compact_boundary_with_metadata() {
        let line = r#"{"type":"system","subtype":"compact_boundary","session_id":"abc","compact_metadata":{"pre_tokens":12345,"trigger":"manual"}}"#;
        let event = parse_line(line).unwrap();
        match event {
            StreamEvent::CompactBoundary { pre_tokens, trigger, session_id } => {
                assert_eq!(session_id, "abc");
                assert_eq!(pre_tokens, Some(12345));
                assert_eq!(trigger, Some("manual".to_string()));
            }
            _ => panic!("Expected CompactBoundary"),
        }
    }

    #[test]
    fn parse_compact_boundary_without_metadata() {
        let line = r#"{"type":"system","subtype":"compact_boundary","session_id":"abc"}"#;
        let event = parse_line(line).unwrap();
        match event {
            StreamEvent::CompactBoundary { pre_tokens, trigger, session_id } => {
                assert_eq!(session_id, "abc");
                assert_eq!(pre_tokens, None);
                assert_eq!(trigger, None);
            }
            _ => panic!("Expected CompactBoundary"),
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
