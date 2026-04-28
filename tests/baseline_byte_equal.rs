//! Baseline byte-equal regression test.
//! Compares format!("{:#?}\n", parse_line(fixture)) against pre-change captures.
//!
//! Wave 1.1에서 캡처한 35개 baseline 파일과 신규 parse_line 출력을 byte-equal 비교한다.
//! 회귀 0 검증의 결정적 게이트.

use pidory::subprocess::parser::parse_line;

fn check(fixture: &str, baseline: &str, name: &str) {
    let actual = format!("{:#?}\n", parse_line(fixture));
    assert_eq!(
        actual,
        baseline,
        "\n=== Regression in {} ===\n--- Expected (baseline) ---\n{}\n--- Actual ---\n{}\n",
        name,
        baseline,
        actual
    );
}

// ============================================================
// init
// ============================================================

#[test]
fn init_basic() {
    check(
        include_str!("fixtures/stream_events/init/basic.json"),
        include_str!("fixtures/stream_events_baseline/init_basic.txt"),
        "init_basic",
    );
}

#[test]
fn init_with_skills() {
    check(
        include_str!("fixtures/stream_events/init/with_skills.json"),
        include_str!("fixtures/stream_events_baseline/init_with_skills.txt"),
        "init_with_skills",
    );
}

#[test]
fn init_without_skills() {
    check(
        include_str!("fixtures/stream_events/init/without_skills.json"),
        include_str!("fixtures/stream_events_baseline/init_without_skills.txt"),
        "init_without_skills",
    );
}

// ============================================================
// assistant
// ============================================================

#[test]
fn assistant_text() {
    check(
        include_str!("fixtures/stream_events/assistant/text.json"),
        include_str!("fixtures/stream_events_baseline/assistant_text.txt"),
        "assistant_text",
    );
}

#[test]
fn assistant_tool_use() {
    check(
        include_str!("fixtures/stream_events/assistant/tool_use.json"),
        include_str!("fixtures/stream_events_baseline/assistant_tool_use.txt"),
        "assistant_tool_use",
    );
}

#[test]
fn assistant_thinking() {
    check(
        include_str!("fixtures/stream_events/assistant/thinking.json"),
        include_str!("fixtures/stream_events_baseline/assistant_thinking.txt"),
        "assistant_thinking",
    );
}

// ============================================================
// user
// ============================================================

#[test]
fn user_tool_result() {
    check(
        include_str!("fixtures/stream_events/user/tool_result.json"),
        include_str!("fixtures/stream_events_baseline/user_tool_result.txt"),
        "user_tool_result",
    );
}

#[test]
fn user_replay_vs_tool_result() {
    check(
        include_str!("fixtures/stream_events/user/vs_replay.json"),
        include_str!("fixtures/stream_events_baseline/user_replay_vs_tool_result.txt"),
        "user_replay_vs_tool_result",
    );
}

// ============================================================
// user_replay
// ============================================================

#[test]
fn user_replay_basic() {
    check(
        include_str!("fixtures/stream_events/user_replay/basic.json"),
        include_str!("fixtures/stream_events_baseline/user_replay_basic.txt"),
        "user_replay_basic",
    );
}

#[test]
fn user_replay_session_id() {
    check(
        include_str!("fixtures/stream_events/user_replay/with_session_id.json"),
        include_str!("fixtures/stream_events_baseline/user_replay_session_id.txt"),
        "user_replay_session_id",
    );
}

// ============================================================
// rate_limit
// ============================================================

#[test]
fn rate_limit_basic() {
    check(
        include_str!("fixtures/stream_events/rate_limit/basic.json"),
        include_str!("fixtures/stream_events_baseline/rate_limit_basic.txt"),
        "rate_limit_basic",
    );
}

#[test]
fn rate_limit_with_utilization() {
    check(
        include_str!("fixtures/stream_events/rate_limit/with_utilization.json"),
        include_str!("fixtures/stream_events_baseline/rate_limit_with_utilization.txt"),
        "rate_limit_with_utilization",
    );
}

#[test]
fn rate_limit_five_hour() {
    check(
        include_str!("fixtures/stream_events/rate_limit/five_hour.json"),
        include_str!("fixtures/stream_events_baseline/rate_limit_five_hour.txt"),
        "rate_limit_five_hour",
    );
}

// ============================================================
// result
// ============================================================

#[test]
fn result_success() {
    check(
        include_str!("fixtures/stream_events/result/success.json"),
        include_str!("fixtures/stream_events_baseline/result_success.txt"),
        "result_success",
    );
}

#[test]
fn result_error() {
    check(
        include_str!("fixtures/stream_events/result/error.json"),
        include_str!("fixtures/stream_events_baseline/result_error.txt"),
        "result_error",
    );
}

#[test]
fn result_with_model_usage() {
    check(
        include_str!("fixtures/stream_events/result/with_model_usage.json"),
        include_str!("fixtures/stream_events_baseline/result_with_model_usage.txt"),
        "result_with_model_usage",
    );
}

#[test]
fn result_multiple_models() {
    check(
        include_str!("fixtures/stream_events/result/multiple_models.json"),
        include_str!("fixtures/stream_events_baseline/result_multiple_models.txt"),
        "result_multiple_models",
    );
}

#[test]
fn result_model_without_cache_fields() {
    check(
        include_str!("fixtures/stream_events/result/model_without_cache.json"),
        include_str!("fixtures/stream_events_baseline/result_model_without_cache_fields.txt"),
        "result_model_without_cache_fields",
    );
}

#[test]
fn result_without_cache_fields() {
    check(
        include_str!("fixtures/stream_events/result/without_cache.json"),
        include_str!("fixtures/stream_events_baseline/result_without_cache_fields.txt"),
        "result_without_cache_fields",
    );
}

#[test]
fn result_empty_model_usage() {
    check(
        include_str!("fixtures/stream_events/result/empty_model_usage.json"),
        include_str!("fixtures/stream_events_baseline/result_empty_model_usage.txt"),
        "result_empty_model_usage",
    );
}

// ============================================================
// control_request
// ============================================================

#[test]
fn control_request_full() {
    check(
        include_str!("fixtures/stream_events/control_request/full.json"),
        include_str!("fixtures/stream_events_baseline/control_request_full.txt"),
        "control_request_full",
    );
}

#[test]
fn control_request_minimal() {
    check(
        include_str!("fixtures/stream_events/control_request/minimal.json"),
        include_str!("fixtures/stream_events_baseline/control_request_minimal.txt"),
        "control_request_minimal",
    );
}

#[test]
fn control_request_no_decision_reason() {
    check(
        include_str!("fixtures/stream_events/control_request/no_decision_reason.json"),
        include_str!("fixtures/stream_events_baseline/control_request_no_decision_reason.txt"),
        "control_request_no_decision_reason",
    );
}

// ============================================================
// task_started
// ============================================================

#[test]
fn task_started_basic() {
    check(
        include_str!("fixtures/stream_events/task_started/basic.json"),
        include_str!("fixtures/stream_events_baseline/task_started_basic.txt"),
        "task_started_basic",
    );
}

#[test]
fn task_started_agent_type() {
    check(
        include_str!("fixtures/stream_events/task_started/agent_type.json"),
        include_str!("fixtures/stream_events_baseline/task_started_agent_type.txt"),
        "task_started_agent_type",
    );
}

#[test]
fn task_started_missing_fields() {
    check(
        include_str!("fixtures/stream_events/task_started/missing_fields.json"),
        include_str!("fixtures/stream_events_baseline/task_started_missing_fields.txt"),
        "task_started_missing_fields",
    );
}

// ============================================================
// task_progress
// ============================================================

#[test]
fn task_progress_basic() {
    check(
        include_str!("fixtures/stream_events/task_progress/basic.json"),
        include_str!("fixtures/stream_events_baseline/task_progress_basic.txt"),
        "task_progress_basic",
    );
}

#[test]
fn task_progress_no_last_tool_name() {
    check(
        include_str!("fixtures/stream_events/task_progress/no_last_tool_name.json"),
        include_str!("fixtures/stream_events_baseline/task_progress_no_last_tool_name.txt"),
        "task_progress_no_last_tool_name",
    );
}

// ============================================================
// task_notification
// ============================================================

#[test]
fn task_notification_completed() {
    check(
        include_str!("fixtures/stream_events/task_notification/completed.json"),
        include_str!("fixtures/stream_events_baseline/task_notification_completed.txt"),
        "task_notification_completed",
    );
}

#[test]
fn task_notification_failed_no_output_file() {
    check(
        include_str!("fixtures/stream_events/task_notification/failed_no_output_file.json"),
        include_str!("fixtures/stream_events_baseline/task_notification_failed_no_output_file.txt"),
        "task_notification_failed_no_output_file",
    );
}

// ============================================================
// compact_boundary
// ============================================================

#[test]
fn compact_boundary_with_metadata() {
    check(
        include_str!("fixtures/stream_events/compact_boundary/with_metadata.json"),
        include_str!("fixtures/stream_events_baseline/compact_boundary_with_metadata.txt"),
        "compact_boundary_with_metadata",
    );
}

#[test]
fn compact_boundary_without_metadata() {
    check(
        include_str!("fixtures/stream_events/compact_boundary/without_metadata.json"),
        include_str!("fixtures/stream_events_baseline/compact_boundary_without_metadata.txt"),
        "compact_boundary_without_metadata",
    );
}

// ============================================================
// edge cases
// ============================================================

#[test]
fn edge_unknown_type() {
    check(
        include_str!("fixtures/stream_events/edge/unknown_type.json"),
        include_str!("fixtures/stream_events_baseline/edge_unknown_type.txt"),
        "edge_unknown_type",
    );
}

#[test]
fn edge_empty() {
    check(
        include_str!("fixtures/stream_events/edge/empty.txt"),
        include_str!("fixtures/stream_events_baseline/edge_empty.txt"),
        "edge_empty",
    );
}

#[test]
fn edge_malformed() {
    check(
        include_str!("fixtures/stream_events/edge/malformed.txt"),
        include_str!("fixtures/stream_events_baseline/edge_malformed.txt"),
        "edge_malformed",
    );
}

// ============================================================
// silent-default fallback regression gates
//
// Anti-regression for review #274 [w1]-[w6]: a single unknown content block
// or wrong-shape Optional field must NOT collapse the whole event into Unknown.
// ============================================================

#[test]
fn assistant_unknown_block_mixed() {
    // Unknown content block (image) is silently skipped; remaining text survives.
    // Without [w1]/[w3] fix: whole Assistant event would fall to Unknown.
    check(
        include_str!("fixtures/stream_events/assistant/unknown_block_mixed.json"),
        include_str!("fixtures/stream_events_baseline/assistant_unknown_block_mixed.txt"),
        "assistant_unknown_block_mixed",
    );
}

#[test]
fn assistant_missing_message() {
    // Missing `message` field → empty content vec, not Unknown.
    // Without [w3] fix: whole Assistant event would fall to Unknown.
    check(
        include_str!("fixtures/stream_events/assistant/missing_message.json"),
        include_str!("fixtures/stream_events_baseline/assistant_missing_message.txt"),
        "assistant_missing_message",
    );
}

#[test]
fn user_unknown_content_mixed() {
    // Unknown user content block is silently skipped; tool_result survives.
    // Without [w2]/[w4] fix: whole User event would fall to Unknown.
    check(
        include_str!("fixtures/stream_events/user/unknown_content_mixed.json"),
        include_str!("fixtures/stream_events_baseline/user_unknown_content_mixed.txt"),
        "user_unknown_content_mixed",
    );
}

#[test]
fn control_request_missing_request() {
    // Missing `request` field → ControlRequest with empty inner, not Unknown.
    // Without [w5] fix: permission flow would silently break.
    check(
        include_str!("fixtures/stream_events/control_request/missing_request.json"),
        include_str!("fixtures/stream_events_baseline/control_request_missing_request.txt"),
        "control_request_missing_request",
    );
}

#[test]
fn rate_limit_wrong_shape_optional() {
    // Wrong-shape Optional fields (resetsAt as string, etc.) silently become None.
    // Without [w6] fix: whole RateLimit event would fall to Unknown.
    check(
        include_str!("fixtures/stream_events/rate_limit/wrong_shape_optional.json"),
        include_str!("fixtures/stream_events_baseline/rate_limit_wrong_shape_optional.txt"),
        "rate_limit_wrong_shape_optional",
    );
}
