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
        skills: Vec<String>,
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
        rate_limit_type: Option<String>,
        utilization: Option<f64>,
        is_using_overage: Option<bool>,
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
        input_tokens: u64,
        output_tokens: u64,
        context_window: u64,
        total_input_tokens: u64,
    },
    UserReplay {
        content: String,
        session_id: String,
        timestamp: Option<String>,
    },
    ControlRequest {
        request_id: String,
        tool_name: String,
        tool_use_id: String,
        input: Value,
        decision_reason: Option<String>,
    },
    TaskStarted {
        task_id: String,
        tool_use_id: String,
        description: String,
        task_type: String,
        session_id: String,
    },
    TaskProgress {
        task_id: String,
        tool_use_id: String,
        description: String,
        last_tool_name: Option<String>,
        session_id: String,
    },
    TaskNotification {
        task_id: String,
        tool_use_id: String,
        status: String,
        summary: String,
        output_file: Option<String>,
        session_id: String,
    },
    CompactBoundary {
        pre_tokens: Option<u64>,
        trigger: Option<String>,
        session_id: String,
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
            StreamEvent::UserReplay { session_id, .. } => Some(session_id),
            StreamEvent::ControlRequest { .. } => None,
            StreamEvent::TaskStarted { session_id, .. } => Some(session_id),
            StreamEvent::TaskProgress { session_id, .. } => Some(session_id),
            StreamEvent::TaskNotification { session_id, .. } => Some(session_id),
            StreamEvent::CompactBoundary { session_id, .. } => Some(session_id),
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

    pub fn is_user_replay(&self) -> bool {
        matches!(self, StreamEvent::UserReplay { .. })
    }

    pub fn is_control_request(&self) -> bool {
        matches!(self, StreamEvent::ControlRequest { .. })
    }

    pub fn is_result(&self) -> bool {
        matches!(self, StreamEvent::Result { .. })
    }

    pub fn is_task_notification(&self) -> bool {
        matches!(self, StreamEvent::TaskNotification { .. })
    }

    pub fn is_error(&self) -> bool {
        if let StreamEvent::Result { is_error, .. } = self {
            *is_error
        } else {
            false
        }
    }
}
