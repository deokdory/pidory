//! Fixture-based integration tests for parse_line.
//!
//! Each fixture in tests/fixtures/stream_events/<variant>/<scenario>.json is parsed
//! via the real lib.rs entry point (pidory::subprocess::parser).
//! Edge cases live in tests/fixtures/stream_events/edge/.
//!
//! Uses include_str! so fixture-file deletion causes a compile error, not a silent pass.

use pidory::subprocess::parser::{ContentBlock, StreamEvent, parse_line};
use serde_json::Value;

// --- init ---
const INIT_BASIC: &str =
    include_str!("fixtures/stream_events/init/basic.json");
const INIT_WITH_SKILLS: &str =
    include_str!("fixtures/stream_events/init/with_skills.json");
const INIT_WITHOUT_SKILLS: &str =
    include_str!("fixtures/stream_events/init/without_skills.json");

// --- assistant ---
const ASSISTANT_TEXT: &str =
    include_str!("fixtures/stream_events/assistant/text.json");
const ASSISTANT_THINKING: &str =
    include_str!("fixtures/stream_events/assistant/thinking.json");
const ASSISTANT_TOOL_USE: &str =
    include_str!("fixtures/stream_events/assistant/tool_use.json");

// --- user ---
const USER_TOOL_RESULT: &str =
    include_str!("fixtures/stream_events/user/tool_result.json");
const USER_VS_REPLAY: &str =
    include_str!("fixtures/stream_events/user/vs_replay.json");

// --- user_replay ---
const USER_REPLAY_BASIC: &str =
    include_str!("fixtures/stream_events/user_replay/basic.json");
const USER_REPLAY_WITH_SESSION_ID: &str =
    include_str!("fixtures/stream_events/user_replay/with_session_id.json");

// --- rate_limit ---
const RATE_LIMIT_BASIC: &str =
    include_str!("fixtures/stream_events/rate_limit/basic.json");
const RATE_LIMIT_FIVE_HOUR: &str =
    include_str!("fixtures/stream_events/rate_limit/five_hour.json");
const RATE_LIMIT_WITH_UTILIZATION: &str =
    include_str!("fixtures/stream_events/rate_limit/with_utilization.json");

// --- result ---
const RESULT_SUCCESS: &str =
    include_str!("fixtures/stream_events/result/success.json");
const RESULT_ERROR: &str =
    include_str!("fixtures/stream_events/result/error.json");
const RESULT_WITH_MODEL_USAGE: &str =
    include_str!("fixtures/stream_events/result/with_model_usage.json");
const RESULT_MULTIPLE_MODELS: &str =
    include_str!("fixtures/stream_events/result/multiple_models.json");
const RESULT_MODEL_WITHOUT_CACHE: &str =
    include_str!("fixtures/stream_events/result/model_without_cache.json");
const RESULT_WITHOUT_CACHE: &str =
    include_str!("fixtures/stream_events/result/without_cache.json");
const RESULT_EMPTY_MODEL_USAGE: &str =
    include_str!("fixtures/stream_events/result/empty_model_usage.json");

// --- control_request ---
const CONTROL_REQUEST_FULL: &str =
    include_str!("fixtures/stream_events/control_request/full.json");
const CONTROL_REQUEST_MINIMAL: &str =
    include_str!("fixtures/stream_events/control_request/minimal.json");
const CONTROL_REQUEST_NO_DECISION_REASON: &str =
    include_str!("fixtures/stream_events/control_request/no_decision_reason.json");

// --- task_started ---
const TASK_STARTED_BASIC: &str =
    include_str!("fixtures/stream_events/task_started/basic.json");
const TASK_STARTED_AGENT_TYPE: &str =
    include_str!("fixtures/stream_events/task_started/agent_type.json");
const TASK_STARTED_MISSING_FIELDS: &str =
    include_str!("fixtures/stream_events/task_started/missing_fields.json");

// --- task_progress ---
const TASK_PROGRESS_BASIC: &str =
    include_str!("fixtures/stream_events/task_progress/basic.json");
const TASK_PROGRESS_NO_LAST_TOOL_NAME: &str =
    include_str!("fixtures/stream_events/task_progress/no_last_tool_name.json");

// --- task_notification ---
const TASK_NOTIFICATION_COMPLETED: &str =
    include_str!("fixtures/stream_events/task_notification/completed.json");
const TASK_NOTIFICATION_FAILED_NO_OUTPUT_FILE: &str =
    include_str!("fixtures/stream_events/task_notification/failed_no_output_file.json");

// --- compact_boundary ---
const COMPACT_BOUNDARY_WITH_METADATA: &str =
    include_str!("fixtures/stream_events/compact_boundary/with_metadata.json");
const COMPACT_BOUNDARY_WITHOUT_METADATA: &str =
    include_str!("fixtures/stream_events/compact_boundary/without_metadata.json");

// --- edge ---
const EDGE_EMPTY: &str =
    include_str!("fixtures/stream_events/edge/empty.txt");
const EDGE_MALFORMED: &str =
    include_str!("fixtures/stream_events/edge/malformed.txt");
const EDGE_UNKNOWN_TYPE: &str =
    include_str!("fixtures/stream_events/edge/unknown_type.json");

// ============================================================
// init
// ============================================================

#[test]
fn init_basic() {
    let event = parse_line(INIT_BASIC).unwrap();
    let StreamEvent::Init { session_id, cwd, tools, model, skills } = event else {
        panic!("expected Init, got {:?}", event);
    };
    assert_eq!(session_id, "abc-123");
    assert_eq!(cwd, "/tmp");
    assert_eq!(tools, vec!["Bash", "Read"]);
    assert_eq!(model, "claude-opus-4-6");
    assert_eq!(skills, vec!["craft", "verify"]);
}

#[test]
fn init_with_skills() {
    let event = parse_line(INIT_WITH_SKILLS).unwrap();
    let StreamEvent::Init { session_id, skills, model, .. } = event else {
        panic!("expected Init, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(model, "claude-opus-4-6");
    assert_eq!(skills, vec!["craft", "verify", "build"]);
}

#[test]
fn init_without_skills() {
    let event = parse_line(INIT_WITHOUT_SKILLS).unwrap();
    let StreamEvent::Init { session_id, skills, model, tools, .. } = event else {
        panic!("expected Init, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(model, "opus");
    assert_eq!(tools, vec!["Bash"]);
    assert!(skills.is_empty(), "skills should be empty when missing from JSON");
}

// ============================================================
// assistant
// ============================================================

#[test]
fn assistant_text() {
    let event = parse_line(ASSISTANT_TEXT).unwrap();
    let StreamEvent::Assistant { content, session_id } = event else {
        panic!("expected Assistant, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(content.len(), 1);
    let ContentBlock::Text(text) = &content[0] else {
        panic!("expected Text block, got {:?}", content[0]);
    };
    assert_eq!(text, "Hello!");
}

#[test]
fn assistant_thinking() {
    let event = parse_line(ASSISTANT_THINKING).unwrap();
    let StreamEvent::Assistant { content, session_id } = event else {
        panic!("expected Assistant, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(content.len(), 1);
    let ContentBlock::Thinking(thought) = &content[0] else {
        panic!("expected Thinking block, got {:?}", content[0]);
    };
    assert_eq!(thought, "let me think...");
}

#[test]
fn assistant_tool_use() {
    let event = parse_line(ASSISTANT_TOOL_USE).unwrap();
    let StreamEvent::Assistant { content, session_id } = event else {
        panic!("expected Assistant, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(content.len(), 1);
    let ContentBlock::ToolUse { id, name, input } = &content[0] else {
        panic!("expected ToolUse block, got {:?}", content[0]);
    };
    assert_eq!(id, "t1");
    assert_eq!(name, "Bash");
    assert_eq!(input["command"], Value::String("echo hi".to_string()));
}

// ============================================================
// user
// ============================================================

#[test]
fn user_tool_result() {
    let event = parse_line(USER_TOOL_RESULT).unwrap();
    let StreamEvent::User { tool_results, session_id } = event else {
        panic!("expected User, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].tool_use_id, "t1");
    assert_eq!(tool_results[0].content, "hello");
    assert!(!tool_results[0].is_error);
}

#[test]
fn user_vs_replay() {
    // vs_replay fixture is a User (not replay) event — verifies discrimination logic
    let event = parse_line(USER_VS_REPLAY).unwrap();
    let StreamEvent::User { tool_results, session_id } = event else {
        panic!("expected User (not UserReplay), got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].tool_use_id, "t1");
}

// ============================================================
// user_replay
// ============================================================

#[test]
fn user_replay_basic() {
    let event = parse_line(USER_REPLAY_BASIC).unwrap();
    let StreamEvent::UserReplay { content, session_id, timestamp } = event else {
        panic!("expected UserReplay, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(content, "say hello");
    assert_eq!(timestamp.as_deref(), Some("2026-03-31T00:00:00Z"));
}

#[test]
fn user_replay_with_session_id() {
    let event = parse_line(USER_REPLAY_WITH_SESSION_ID).unwrap();
    let StreamEvent::UserReplay { content, session_id, timestamp } = event else {
        panic!("expected UserReplay, got {:?}", event);
    };
    assert_eq!(session_id, "my-session");
    assert_eq!(content, "hi");
    assert!(timestamp.is_none());
}

// ============================================================
// rate_limit
// ============================================================

#[test]
fn rate_limit_basic() {
    let event = parse_line(RATE_LIMIT_BASIC).unwrap();
    let StreamEvent::RateLimit { status, resets_at, session_id, rate_limit_type, utilization, is_using_overage } = event else {
        panic!("expected RateLimit, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(status, "allowed");
    assert_eq!(resets_at, Some(12345));
    assert!(rate_limit_type.is_none());
    assert!(utilization.is_none());
    assert!(is_using_overage.is_none());
}

#[test]
fn rate_limit_five_hour() {
    let event = parse_line(RATE_LIMIT_FIVE_HOUR).unwrap();
    let StreamEvent::RateLimit { status, rate_limit_type, utilization, is_using_overage, .. } = event else {
        panic!("expected RateLimit, got {:?}", event);
    };
    assert_eq!(status, "allowed");
    assert_eq!(rate_limit_type.as_deref(), Some("five_hour"));
    assert!((utilization.unwrap() - 0.24).abs() < 1e-10);
    assert_eq!(is_using_overage, Some(false));
}

#[test]
fn rate_limit_with_utilization() {
    let event = parse_line(RATE_LIMIT_WITH_UTILIZATION).unwrap();
    let StreamEvent::RateLimit { status, rate_limit_type, utilization, resets_at, .. } = event else {
        panic!("expected RateLimit, got {:?}", event);
    };
    assert_eq!(status, "allowed_warning");
    assert_eq!(rate_limit_type.as_deref(), Some("seven_day"));
    assert!((utilization.unwrap() - 0.57).abs() < 1e-10);
    assert_eq!(resets_at, Some(1776042000));
}

// ============================================================
// result
// ============================================================

#[test]
fn result_success() {
    let event = parse_line(RESULT_SUCCESS).unwrap();
    let StreamEvent::Result { subtype, session_id, is_error, result, duration_ms, total_cost_usd, num_turns, .. } = event else {
        panic!("expected Result, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(subtype, "success");
    assert!(!is_error);
    assert_eq!(result.as_deref(), Some("done"));
    assert_eq!(duration_ms, 1000);
    assert!((total_cost_usd - 0.05).abs() < 1e-10);
    assert_eq!(num_turns, 1);
}

#[test]
fn result_error() {
    let event = parse_line(RESULT_ERROR).unwrap();
    let StreamEvent::Result { subtype, session_id, is_error, errors, .. } = event else {
        panic!("expected Result, got {:?}", event);
    };
    assert_eq!(session_id, "xyz");
    assert_eq!(subtype, "error_during_execution");
    assert!(is_error);
    assert_eq!(errors, vec!["No conversation found"]);
}

#[test]
fn result_with_model_usage() {
    let event = parse_line(RESULT_WITH_MODEL_USAGE).unwrap();
    let StreamEvent::Result { context_window, input_tokens, total_input_tokens, output_tokens, .. } = event else {
        panic!("expected Result, got {:?}", event);
    };
    assert_eq!(context_window, 1_000_000);
    assert_eq!(input_tokens, 3);
    assert_eq!(output_tokens, 4);
    // total_input_tokens = input_tokens(3) + cache_creation(26147) + cache_read(0)
    assert_eq!(total_input_tokens, 26150);
}

#[test]
fn result_multiple_models() {
    let event = parse_line(RESULT_MULTIPLE_MODELS).unwrap();
    let StreamEvent::Result { context_window, num_turns, total_input_tokens, .. } = event else {
        panic!("expected Result, got {:?}", event);
    };
    // largest contextWindow among models is 1_000_000
    assert_eq!(context_window, 1_000_000);
    assert_eq!(num_turns, 3);
    // total_input_tokens = input_tokens(17) + cache_creation(263893) + cache_read(1667176)
    assert_eq!(total_input_tokens, 1_931_086);
}

#[test]
fn result_model_without_cache() {
    let event = parse_line(RESULT_MODEL_WITHOUT_CACHE).unwrap();
    let StreamEvent::Result { context_window, input_tokens, total_input_tokens, .. } = event else {
        panic!("expected Result, got {:?}", event);
    };
    assert_eq!(context_window, 200_000);
    assert_eq!(input_tokens, 10);
    assert_eq!(total_input_tokens, 10);
}

#[test]
fn result_without_cache() {
    let event = parse_line(RESULT_WITHOUT_CACHE).unwrap();
    let StreamEvent::Result { input_tokens, output_tokens, total_input_tokens, context_window, .. } = event else {
        panic!("expected Result, got {:?}", event);
    };
    assert_eq!(input_tokens, 500);
    assert_eq!(output_tokens, 100);
    // no cache fields → total_input_tokens == input_tokens
    assert_eq!(total_input_tokens, 500);
    // no modelUsage → context_window defaults to 0
    assert_eq!(context_window, 0);
}

#[test]
fn result_empty_model_usage() {
    let event = parse_line(RESULT_EMPTY_MODEL_USAGE).unwrap();
    let StreamEvent::Result { context_window, num_turns, is_error, .. } = event else {
        panic!("expected Result, got {:?}", event);
    };
    // empty modelUsage object → no max → context_window = 0
    assert_eq!(context_window, 0);
    assert_eq!(num_turns, 0);
    assert!(!is_error);
}

// ============================================================
// control_request
// ============================================================

#[test]
fn control_request_full() {
    let event = parse_line(CONTROL_REQUEST_FULL).unwrap();
    let StreamEvent::ControlRequest { request_id, tool_name, tool_use_id, input, decision_reason } = event else {
        panic!("expected ControlRequest, got {:?}", event);
    };
    assert_eq!(request_id, "e5c3058b-6794-4a0d-b445-7729855cb810");
    assert_eq!(tool_name, "Write");
    assert_eq!(tool_use_id, "toolu_01BKN27SrcApvHEMYi7A1ik4");
    assert_eq!(input["file_path"], Value::String("/tmp/test.txt".to_string()));
    assert_eq!(
        decision_reason.as_deref(),
        Some("Path is outside allowed working directories")
    );
}

#[test]
fn control_request_minimal() {
    let event = parse_line(CONTROL_REQUEST_MINIMAL).unwrap();
    let StreamEvent::ControlRequest { request_id, tool_name, tool_use_id, decision_reason, .. } = event else {
        panic!("expected ControlRequest, got {:?}", event);
    };
    assert_eq!(request_id, "abc");
    assert_eq!(tool_name, "Bash");
    assert_eq!(tool_use_id, "t1");
    assert!(decision_reason.is_none());
}

#[test]
fn control_request_no_decision_reason() {
    let event = parse_line(CONTROL_REQUEST_NO_DECISION_REASON).unwrap();
    let StreamEvent::ControlRequest { tool_name, decision_reason, .. } = event else {
        panic!("expected ControlRequest, got {:?}", event);
    };
    assert_eq!(tool_name, "Read");
    assert!(decision_reason.is_none());
}

// ============================================================
// task_started
// ============================================================

#[test]
fn task_started_basic() {
    let event = parse_line(TASK_STARTED_BASIC).unwrap();
    let StreamEvent::TaskStarted { task_id, tool_use_id, description, task_type, session_id } = event else {
        panic!("expected TaskStarted, got {:?}", event);
    };
    assert_eq!(task_id, "buyj7z5o7");
    assert_eq!(tool_use_id, "toolu_012daJxCZsawPJKJYF6WxmtC");
    assert_eq!(description, "Sleep 3 seconds then print bg_task_done");
    assert_eq!(task_type, "local_bash");
    assert_eq!(session_id, "40746b0a-41c2-4f62-9ea6-d683612ad9ae");
}

#[test]
fn task_started_agent_type() {
    let event = parse_line(TASK_STARTED_AGENT_TYPE).unwrap();
    let StreamEvent::TaskStarted { task_id, task_type, session_id, .. } = event else {
        panic!("expected TaskStarted, got {:?}", event);
    };
    assert_eq!(task_id, "a7ca5a342d867c971");
    assert_eq!(task_type, "local_agent");
    assert_eq!(session_id, "a352a7c9-4254-465e-b444-b804c6099892");
}

#[test]
fn task_started_missing_fields() {
    let event = parse_line(TASK_STARTED_MISSING_FIELDS).unwrap();
    let StreamEvent::TaskStarted { task_id, tool_use_id, description, task_type, session_id } = event else {
        panic!("expected TaskStarted, got {:?}", event);
    };
    assert_eq!(task_id, "t1");
    assert_eq!(session_id, "s1");
    // missing fields default to empty string
    assert_eq!(tool_use_id, "");
    assert_eq!(description, "");
    assert_eq!(task_type, "");
}

// ============================================================
// task_progress
// ============================================================

#[test]
fn task_progress_basic() {
    let event = parse_line(TASK_PROGRESS_BASIC).unwrap();
    let StreamEvent::TaskProgress { task_id, tool_use_id, description, last_tool_name, session_id } = event else {
        panic!("expected TaskProgress, got {:?}", event);
    };
    assert_eq!(task_id, "a7ca5a342d867c971");
    assert_eq!(tool_use_id, "toolu_01CWFNoUWUwyfyMqVJ42F96Z");
    assert_eq!(description, "Reading /etc/hostname");
    assert_eq!(last_tool_name.as_deref(), Some("Read"));
    assert_eq!(session_id, "a352a7c9-4254-465e-b444-b804c6099892");
}

#[test]
fn task_progress_no_last_tool_name() {
    let event = parse_line(TASK_PROGRESS_NO_LAST_TOOL_NAME).unwrap();
    let StreamEvent::TaskProgress { task_id, last_tool_name, session_id, .. } = event else {
        panic!("expected TaskProgress, got {:?}", event);
    };
    assert_eq!(task_id, "t1");
    assert_eq!(session_id, "s1");
    assert!(last_tool_name.is_none());
}

// ============================================================
// task_notification
// ============================================================

#[test]
fn task_notification_completed() {
    let event = parse_line(TASK_NOTIFICATION_COMPLETED).unwrap();
    let StreamEvent::TaskNotification { task_id, status, summary, output_file, session_id, .. } = event else {
        panic!("expected TaskNotification, got {:?}", event);
    };
    assert_eq!(task_id, "buyj7z5o7");
    assert_eq!(status, "completed");
    assert_eq!(summary, "Background command completed (exit code 0)");
    assert_eq!(
        output_file.as_deref(),
        Some("/tmp/claude-1000/tasks/buyj7z5o7.output")
    );
    assert_eq!(session_id, "40746b0a-41c2-4f62-9ea6-d683612ad9ae");
}

#[test]
fn task_notification_failed_no_output_file() {
    let event = parse_line(TASK_NOTIFICATION_FAILED_NO_OUTPUT_FILE).unwrap();
    let StreamEvent::TaskNotification { task_id, status, output_file, session_id, .. } = event else {
        panic!("expected TaskNotification, got {:?}", event);
    };
    assert_eq!(task_id, "bf2vp1kx2");
    assert_eq!(status, "failed");
    assert!(output_file.is_none());
    assert_eq!(session_id, "sess-xyz");
}

// ============================================================
// compact_boundary
// ============================================================

#[test]
fn compact_boundary_with_metadata() {
    let event = parse_line(COMPACT_BOUNDARY_WITH_METADATA).unwrap();
    let StreamEvent::CompactBoundary { pre_tokens, trigger, session_id } = event else {
        panic!("expected CompactBoundary, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert_eq!(pre_tokens, Some(12345));
    assert_eq!(trigger.as_deref(), Some("manual"));
}

#[test]
fn compact_boundary_without_metadata() {
    let event = parse_line(COMPACT_BOUNDARY_WITHOUT_METADATA).unwrap();
    let StreamEvent::CompactBoundary { pre_tokens, trigger, session_id } = event else {
        panic!("expected CompactBoundary, got {:?}", event);
    };
    assert_eq!(session_id, "abc");
    assert!(pre_tokens.is_none());
    assert!(trigger.is_none());
}

// ============================================================
// edge cases
// ============================================================

#[test]
fn edge_empty() {
    // empty line → Unknown { raw: Value::Null }
    let event = parse_line(EDGE_EMPTY).unwrap();
    let StreamEvent::Unknown { raw } = event else {
        panic!("expected Unknown, got {:?}", event);
    };
    assert_eq!(raw, Value::Null);
}

#[test]
fn edge_malformed() {
    // invalid JSON → Err
    let result = parse_line(EDGE_MALFORMED);
    assert!(result.is_err(), "malformed JSON must return Err, got {:?}", result);
}

#[test]
fn edge_unknown_type() {
    // valid JSON but unknown "type" field → Unknown
    let event = parse_line(EDGE_UNKNOWN_TYPE).unwrap();
    let StreamEvent::Unknown { raw } = event else {
        panic!("expected Unknown, got {:?}", event);
    };
    // raw should contain the original JSON object
    assert_eq!(raw["type"], Value::String("stream_event".to_string()));
    assert_eq!(raw["session_id"], Value::String("abc".to_string()));
}
