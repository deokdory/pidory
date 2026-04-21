use super::Lang;

impl Lang {
    // ── Status display ──

    pub fn working(&self) -> &'static str {
        match self {
            Lang::Ko => "작업 중...",
            Lang::En => "Working...",
        }
    }

    pub fn status_error(&self, err: &str) -> String {
        match self {
            Lang::Ko => format!("오류 — {}", err),
            Lang::En => format!("Error — {}", err),
        }
    }

    pub fn more_items(&self, count: usize) -> String {
        match self {
            Lang::Ko => format!("... +{} 더보기", count),
            Lang::En => format!("... +{} more", count),
        }
    }

    // ── Progress indicators ──
    // TODO: Ko/En 문자열 동일 — 한국어 로컬라이제이션 필요 시 분리

    /// Progress: tool 실행 중
    pub fn progress_tool(&self, name: &str, elapsed: &str) -> String {
        match self {
            Lang::Ko => format!("⏱️ {} ({})", name, elapsed),
            Lang::En => format!("⏱️ {} ({})", name, elapsed),
        }
    }

    /// Progress: thinking 중
    pub fn progress_thinking(&self, elapsed: &str) -> String {
        match self {
            Lang::Ko => format!("⏱️ thinking... ({})", elapsed),
            Lang::En => format!("⏱️ thinking... ({})", elapsed),
        }
    }

    /// Progress: tool 완료
    pub fn progress_tool_done(&self, name: &str, elapsed: &str) -> String {
        match self {
            Lang::Ko => format!("⏱️ {} — {}", name, elapsed),
            Lang::En => format!("⏱️ {} — {}", name, elapsed),
        }
    }

    /// Progress: thinking 완료
    pub fn progress_thinking_done(&self, elapsed: &str) -> String {
        match self {
            Lang::Ko => format!("⏱️ thinking — {}", elapsed),
            Lang::En => format!("⏱️ thinking — {}", elapsed),
        }
    }

    // ── Commands: register ──

    pub fn path_not_exist(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!("경로가 존재하지 않습니다: `{}`", path),
            Lang::En => format!("Path does not exist: `{}`", path),
        }
    }

    pub fn already_registered(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!(
                "이 채널은 이미 `{}`에 등록되어 있습니다. `/unregister`를 먼저 실행하세요.",
                path
            ),
            Lang::En => format!(
                "This channel is already registered to `{}`. Use `/unregister` first.",
                path
            ),
        }
    }

    pub fn registered(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!("`{}`이(가) 이 채널에 등록되었습니다", path),
            Lang::En => format!("Registered `{}` to this channel", path),
        }
    }

    pub fn not_registered(&self) -> &'static str {
        match self {
            Lang::Ko => "이 채널에 등록된 프로젝트가 없습니다",
            Lang::En => "No project registered to this channel",
        }
    }

    pub fn unregistered(&self) -> &'static str {
        match self {
            Lang::Ko => "이 채널에서 프로젝트 등록이 해제되었습니다",
            Lang::En => "Unregistered project from this channel",
        }
    }

    // ── Commands: session ──

    pub fn no_active_sessions_short(&self) -> &'static str {
        match self {
            Lang::Ko => "활성 세션 없음",
            Lang::En => "No active sessions",
        }
    }

    pub fn active_sessions_header(&self, count: usize, max: usize) -> String {
        match self {
            Lang::Ko => format!("📊 활성 세션 ({}/{})", count, max),
            Lang::En => format!("📊 Active Sessions ({}/{})", count, max),
        }
    }

    pub fn active_sessions_list_header(&self) -> &'static str {
        match self {
            Lang::Ko => "📋 활성 세션:",
            Lang::En => "📋 Active Sessions:",
        }
    }

    pub fn no_session_in_thread(&self) -> &'static str {
        match self {
            Lang::Ko => "이 스레드에 활성 세션이 없습니다",
            Lang::En => "No active session in this thread",
        }
    }

    pub fn interrupted(&self) -> &'static str {
        match self {
            Lang::Ko => "중단됨",
            Lang::En => "Interrupted",
        }
    }

    pub fn interrupt_failed(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("중단 실패: {}", err),
            Lang::En => format!("Interrupt failed: {}", err),
        }
    }

    pub fn kicked(&self) -> &'static str {
        match self {
            Lang::Ko => "턴을 중단하고 재시작합니다",
            Lang::En => "Turn interrupted, restarting",
        }
    }

    pub fn kick_no_active_turn(&self) -> &'static str {
        match self {
            Lang::Ko => "진행 중인 턴이 없습니다",
            Lang::En => "No active turn",
        }
    }

    pub fn kick_timeout(&self) -> &'static str {
        match self {
            Lang::Ko => "인터럽트 타임아웃: 세션이 응답하지 않습니다",
            Lang::En => "Interrupt timeout: session not responding",
        }
    }

    pub fn kick_cooldown(&self) -> &'static str {
        match self {
            Lang::Ko => "잠시 후 다시 시도하세요 (5초 cooldown)",
            Lang::En => "Please wait before kicking again (5s cooldown)",
        }
    }

    pub fn kick_system_reminder(&self, last_tool: &str) -> String {
        match self {
            Lang::Ko => format!(
                "<system-reminder>\n사용자가 현재 턴을 인터럽트했습니다. 응답이 오래 걸려 뻗은 것으로 판단했습니다.\n마지막 tool use: {}\n이전 작업을 이어서 진행하세요.\n</system-reminder>",
                last_tool
            ),
            Lang::En => format!(
                "<system-reminder>\nThe user interrupted the current turn. They determined the response was taking too long.\nLast tool use: {}\nContinue the previous work.\n</system-reminder>",
                last_tool
            ),
        }
    }

    pub fn kick_natural_completion(&self) -> &'static str {
        match self {
            Lang::Ko => "턴이 이미 완료되어 재시작하지 않습니다",
            Lang::En => "Turn already completed, skipping restart",
        }
    }

    pub fn kick_error_state(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 에러 상태입니다. 재시작하지 않습니다",
            Lang::En => "Session in error state, skipping restart",
        }
    }

    pub fn kick_preempted(&self) -> &'static str {
        match self {
            Lang::Ko => "다른 메시지가 먼저 처리되어 재시작하지 않습니다",
            Lang::En => "Another message was processed first, skipping restart",
        }
    }

    pub fn not_in_thread(&self) -> &'static str {
        match self {
            Lang::Ko => "스레드가 아닙니다. 스레드 ID를 직접 입력하세요.",
            Lang::En => "Not in a thread. Provide a thread ID explicitly.",
        }
    }

    pub fn no_session_found(&self, tid: &str) -> String {
        match self {
            Lang::Ko => format!("스레드 `{}`에 세션이 없습니다", tid),
            Lang::En => format!("No session found for thread `{}`", tid),
        }
    }

    pub fn session_deleted(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 삭제되었습니다",
            Lang::En => "Session deleted",
        }
    }

    pub fn session_status_display(
        &self,
        thread_id: &str,
        status: &str,
        session_id: &str,
        last_active: &str,
        model: &str,
    ) -> String {
        match self {
            Lang::Ko => format!(
                "📊 세션 상태\n스레드: <#{}>\n상태: {}\n모델: {}\n세션 ID: {}\n마지막 활성: {}",
                thread_id, status, model, session_id, last_active
            ),
            Lang::En => format!(
                "📊 Session Status\nThread: <#{}>\nStatus: {}\nModel: {}\nSession ID: {}\nLast Active: {}",
                thread_id, status, model, session_id, last_active
            ),
        }
    }

    pub fn running_status(&self) -> &'static str {
        match self {
            Lang::Ko => "🔄 실행 중",
            Lang::En => "🔄 running",
        }
    }

    pub fn bg_tasks_suffix(&self) -> &'static str {
        match self {
            Lang::Ko => " — 백그라운드 작업",
            Lang::En => " — bg tasks",
        }
    }

    pub fn none_placeholder(&self) -> &'static str {
        match self {
            Lang::Ko => "(없음)",
            Lang::En => "(none)",
        }
    }

    pub fn never_placeholder(&self) -> &'static str {
        match self {
            Lang::Ko => "(없음)",
            Lang::En => "(never)",
        }
    }

    pub fn session_list_row(
        &self,
        thread_mention: &str,
        status: &str,
        session_short: &str,
        since: &str,
    ) -> String {
        match self {
            Lang::Ko => format!(
                "• {} — 상태: {}{}{}",
                thread_mention, status, session_short, since
            ),
            Lang::En => format!(
                "• {} — status: {}{}{}",
                thread_mention, status, session_short, since
            ),
        }
    }

    pub fn session_list_since(&self, relative: &str) -> String {
        match self {
            Lang::Ko => format!(" — 시작: {}", relative),
            Lang::En => format!(" — since: {}", relative),
        }
    }

    pub fn session_list_id(&self, short_id: &str) -> String {
        match self {
            Lang::Ko => format!(" — 세션: {}…", short_id),
            Lang::En => format!(" — session: {}…", short_id),
        }
    }

    // ── Commands: sleep ──

    pub fn session_slept(&self) -> &'static str {
        match self {
            Lang::Ko => "세션을 슬립 상태로 전환했습니다. 메시지를 보내면 자동으로 재개됩니다.",
            Lang::En => "Session put to sleep. Send a message to resume.",
        }
    }

    pub fn sleep_turn_active(&self) -> &'static str {
        match self {
            Lang::Ko => "턴이 진행 중입니다. 완료 후 다시 시도하세요.",
            Lang::En => "A turn is active. Please try again after it completes.",
        }
    }

    // ── Commands: model ──

    pub fn model_changed(&self, from: &str, to: &str) -> String {
        match self {
            Lang::Ko => format!("모델 변경: {} → {} (다음 메시지부터 적용)", from, to),
            Lang::En => format!("Model changed: {} → {} (applies from next message)", from, to),
        }
    }

    pub fn model_current(&self, model: &str) -> String {
        match self {
            Lang::Ko => format!("현재 모델: {}", model),
            Lang::En => format!("Current model: {}", model),
        }
    }

    pub fn model_turn_active(&self) -> &'static str {
        match self {
            Lang::Ko => "턴 진행 중에는 모델을 변경할 수 없습니다",
            Lang::En => "Cannot change model during an active turn",
        }
    }

    pub fn model_invalid(&self, name: &str) -> String {
        match self {
            Lang::Ko => format!("지원하지 않는 모델입니다: {}", name),
            Lang::En => format!("Unsupported model: {}", name),
        }
    }

    // ── Commands: new-project ──

    pub fn new_project_created(&self, channel: &str, path: &str) -> String {
        match self {
            Lang::Ko => format!("채널 <#{}> 이(가) `{}`에 생성되고 등록되었습니다", channel, path),
            Lang::En => format!("Channel <#{}> created and registered to `{}`", channel, path),
        }
    }

    pub fn channel_name_invalid(&self) -> &'static str {
        match self {
            Lang::Ko => "유효하지 않은 채널 이름입니다 (2-100자, 영문/숫자/하이픈)",
            Lang::En => "Invalid channel name (2-100 chars, alphanumeric/hyphen only)",
        }
    }

    pub fn channel_name_specify_hint(&self) -> &'static str {
        match self {
            Lang::Ko => "name 파라미터로 이름을 직접 지정하세요.",
            Lang::En => "Please specify a name using the name parameter.",
        }
    }

    pub fn channel_create_failed(&self) -> &'static str {
        match self {
            Lang::Ko => "채널 생성에 실패했습니다. 봇에 'Manage Channels' 권한이 있는지 확인하세요.",
            Lang::En => "Failed to create channel. Please check that the bot has the 'Manage Channels' permission.",
        }
    }

    pub fn path_not_in_roots(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!("`{}`은(는) 허용된 project_roots 안에 없습니다", path),
            Lang::En => format!("`{}` is not within any allowed project_roots", path),
        }
    }

    pub fn category_not_found(&self) -> &'static str {
        match self {
            Lang::Ko => "지정된 카테고리를 찾을 수 없습니다",
            Lang::En => "Specified category not found",
        }
    }

    pub fn channel_created_but_register_failed(&self, channel: &str) -> String {
        match self {
            Lang::Ko => format!("채널 <#{}>이(가) 생성되었지만 등록에 실패했습니다. `/register`를 수동으로 실행하세요.", channel),
            Lang::En => format!("Channel <#{}> was created but registration failed. Run `/register` manually.", channel),
        }
    }

    // ── Commands: branch ──

    pub fn branch_not_in_thread(&self) -> &'static str {
        match self {
            Lang::Ko => "스레드 안에서만 사용 가능합니다",
            Lang::En => "This command can only be used inside a thread",
        }
    }

    pub fn branch_no_project(&self) -> &'static str {
        match self {
            Lang::Ko => "이 채널에 등록된 프로젝트가 없습니다",
            Lang::En => "No project registered for this channel",
        }
    }

    pub fn branch_no_session(&self) -> &'static str {
        match self {
            Lang::Ko => "이 스레드에 활성 세션이 없습니다",
            Lang::En => "No active session in this thread",
        }
    }

    pub fn branch_session_busy(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 작업 중입니다. 완료 후 다시 시도해주세요",
            Lang::En => "Session is busy. Please try again after it completes",
        }
    }

    pub fn branch_no_slot(&self, reason: &str) -> String {
        match self {
            Lang::Ko => format!("세션 슬롯이 부족합니다: {}. 잠시 후 다시 시도해주세요", reason),
            Lang::En => format!("No session slot available: {}. Please try again later", reason),
        }
    }

    pub fn branch_summary_failed(&self) -> &'static str {
        match self {
            Lang::Ko => "요약 생성에 실패했습니다",
            Lang::En => "Failed to generate summary",
        }
    }

    pub fn branch_thread_created(&self, thread_mention: &str) -> String {
        match self {
            Lang::Ko => format!("새 스레드가 생성되었습니다: {}", thread_mention),
            Lang::En => format!("New thread created: {}", thread_mention),
        }
    }

    pub fn branch_thread_create_failed(&self) -> &'static str {
        match self {
            Lang::Ko => "스레드 생성에 실패했습니다",
            Lang::En => "Failed to create thread",
        }
    }

    pub fn branch_context_header(&self, source_thread: &str) -> String {
        match self {
            Lang::Ko => format!("🔀 {}에서 분기됨", source_thread),
            Lang::En => format!("🔀 Branched from {}", source_thread),
        }
    }

    pub fn branch_ready(&self, mention: &str) -> String {
        match self {
            Lang::Ko => format!("✅ 세션이 준비되었습니다. {} 메시지를 보내면 작업을 시작합니다.", mention),
            Lang::En => format!("✅ Session is ready. {} Send a message to start working.", mention),
        }
    }

    pub fn branch_summary_prompt(&self, extra_context: &str) -> String {
        match self {
            Lang::Ko | Lang::En => {
                if extra_context.is_empty() {
                    "Summarize the current conversation and work context for handoff to a new session.\n\nRespond ONLY with a JSON object, no other text. Do NOT use any tools.\n{\"title\": \"short descriptive title for the new thread (max 50 chars, Korean if conversation is in Korean)\", \"summary\": \"comprehensive summary of current project state, ongoing work, key decisions, and relevant file paths (max 2000 chars)\"}".to_string()
                } else {
                    format!("Summarize the current conversation and work context for handoff to a new session.\nFocus especially on: {}\n\nRespond ONLY with a JSON object, no other text. Do NOT use any tools.\n{{\"title\": \"short descriptive title for the new thread (max 50 chars, Korean if conversation is in Korean)\", \"summary\": \"comprehensive summary of current project state, ongoing work, key decisions, and relevant file paths (max 2000 chars)\"}}", extra_context)
                }
            }
        }
    }

    // ── Commands: agent ──

    pub fn agent_autocomplete_more(&self, count: usize) -> String {
        match self {
            Lang::Ko => format!(
                "\u{22ef} 외 {}개 (클릭하지 마시고 이름을 입력해서 필터링해 주세요)",
                count
            ),
            Lang::En => format!(
                "\u{22ef} {} more (please type the name to filter instead of clicking)",
                count
            ),
        }
    }

    pub fn agent_not_found(&self, name: &str) -> String {
        match self {
            Lang::Ko => format!(
                "❌ '{}' 에이전트를 목록에서 찾지 못했습니다.\n- 이름 철자를 확인해 주세요\n- 또는 강제 실행하려면 `force:true`를 추가해 주세요",
                name
            ),
            Lang::En => format!(
                "❌ Agent '{}' was not found in the list.\n- Please verify the spelling\n- Or add `force:true` to execute anyway",
                name
            ),
        }
    }

    // ── Commands: update ──

    pub fn update_in_progress(&self) -> &'static str {
        match self {
            Lang::Ko => "⏳ 업데이트 진행 중...",
            Lang::En => "⏳ Update in progress...",
        }
    }

    pub fn update_already_latest(&self, version: &str) -> String {
        match self {
            Lang::Ko => format!("✅ 이미 최신 버전 v{}입니다.", version),
            Lang::En => format!("✅ Already on latest v{}.", version),
        }
    }

    pub fn update_dirty_tree(&self) -> &'static str {
        match self {
            Lang::Ko => "❌ 작업 트리에 커밋되지 않은 변경이 있습니다. 먼저 정리하세요.",
            Lang::En => "❌ Working tree has uncommitted changes. Clean up first.",
        }
    }

    pub fn update_active_turns(&self, threads: &[String]) -> String {
        let joined = threads.join(", ");
        match self {
            Lang::Ko => format!(
                "❌ 활성 턴이 있습니다: {}\n`force:true`로 강제 진행 가능합니다.",
                joined
            ),
            Lang::En => format!(
                "❌ Active turns in: {}\nUse `force:true` to override.",
                joined
            ),
        }
    }

    pub fn update_build_failed(&self, stderr_tail: &str) -> String {
        match self {
            Lang::Ko => format!("❌ 빌드 실패:\n```\n{}\n```", stderr_tail),
            Lang::En => format!("❌ Build failed:\n```\n{}\n```", stderr_tail),
        }
    }

    pub fn update_complete(&self, version: &str) -> String {
        match self {
            Lang::Ko => format!("✅ v{} 빌드 완료. 30초 후 재시작합니다.", version),
            Lang::En => format!("✅ v{} built. Restarting in 30 seconds.", version),
        }
    }

    pub fn update_rollback(&self) -> &'static str {
        match self {
            Lang::Ko => "⚠️ 업데이트 후 부팅 실패로 자동 롤백됨.",
            Lang::En => "⚠️ Update failed to boot — auto-rolled back.",
        }
    }
}
