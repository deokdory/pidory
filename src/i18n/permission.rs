use crate::handler::formatter::inline_code;

use super::Lang;

impl Lang {
    // ── Permissions ──

    pub fn permission_request_label(&self) -> &'static str {
        match self {
            Lang::Ko => "실행 허가 요청",
            Lang::En => "Permission request",
        }
    }

    pub fn no_permission(&self) -> &'static str {
        match self {
            Lang::Ko => "권한이 없어요",
            Lang::En => "Permission denied",
        }
    }

    pub fn btn_allow(&self) -> &'static str {
        match self {
            Lang::Ko => "허용",
            Lang::En => "Allow",
        }
    }

    pub fn btn_always_allow(&self) -> &'static str {
        match self {
            Lang::Ko => "항상 허용",
            Lang::En => "Always Allow",
        }
    }

    pub fn btn_deny(&self) -> &'static str {
        match self {
            Lang::Ko => "거부",
            Lang::En => "Deny",
        }
    }

    pub fn perm_allowed(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 허용됨", inline_code(tool)),
            Lang::En => format!("{} — Allowed", inline_code(tool)),
        }
    }

    pub fn perm_always_allowed(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 항상 허용됨", inline_code(tool)),
            Lang::En => format!("{} — Always Allowed", inline_code(tool)),
        }
    }

    pub fn perm_denied(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 거부됨", inline_code(tool)),
            Lang::En => format!("{} — Denied", inline_code(tool)),
        }
    }

    pub fn btn_once(&self) -> &'static str {
        match self {
            Lang::Ko => "한 번만",
            Lang::En => "Allow Once",
        }
    }

    pub fn btn_always_exact(&self) -> &'static str {
        match self {
            Lang::Ko => "이 명령만",
            Lang::En => "This command only",
        }
    }

    pub fn btn_always_prefix(&self) -> &'static str {
        match self {
            Lang::Ko => "같은 prefix",
            Lang::En => "Same prefix",
        }
    }

    pub fn btn_always_domain(&self) -> &'static str {
        match self {
            Lang::Ko => "같은 도메인",
            Lang::En => "Same domain",
        }
    }

    pub fn btn_always_tool(&self) -> &'static str {
        match self {
            Lang::Ko => "⚠️ 도구 전체",
            Lang::En => "⚠️ Entire tool",
        }
    }

    /// 권한 요청 메시지 섹션 헤더 (Discord 제3 레벨 헤더 마크다운)
    pub fn lbl_permission_request_section_header(&self) -> &'static str {
        match self {
            Lang::Ko => "### 권한 요청",
            Lang::En => "### Permission Request",
        }
    }

    /// scope 토글 버튼 — 현재 Project 상태 표시 (클릭 시 Global 전환)
    pub fn btn_scope_status_project(&self) -> &'static str {
        match self {
            Lang::Ko => "📁 적용 범위: 프로젝트",
            Lang::En => "📁 Applied to: project",
        }
    }

    /// scope 토글 버튼 — 현재 Global 상태 표시 (클릭 시 Project 전환)
    pub fn btn_scope_status_global(&self) -> &'static str {
        match self {
            Lang::Ko => "⚠️ 적용 범위: 전역",
            Lang::En => "⚠️ Applied to: global",
        }
    }

    /// 권한 메시지 — `항상 허용` 옵션 섹션 헤더 (Discord Bold 스타일)
    pub fn msg_always_allow_options_header(&self) -> &'static str {
        match self {
            Lang::Ko => "**항상 허용 옵션**",
            Lang::En => "**Always allow options**",
        }
    }

    /// Always Allow 성공 — 프로젝트 범위 (basename 포함, rules 콤마 나열)
    pub fn msg_save_success_project(&self, basename: &str, rules: &str) -> String {
        match self {
            Lang::Ko => format!("✅ {basename}에서 항상 허용됨: {rules}"),
            Lang::En => format!("✅ Always allowed in {basename}: {rules}"),
        }
    }

    /// Always Allow 성공 — 전역 범위 (rules 콤마 나열)
    pub fn msg_save_success_global(&self, rules: &str) -> String {
        match self {
            Lang::Ko => format!("✅ 모든 프로젝트에서 항상 허용됨: {rules}"),
            Lang::En => format!("✅ Always allowed in all projects: {rules}"),
        }
    }

    /// 프로젝트 basename 조회 실패 시 fallback 표시
    pub fn msg_project_basename_fallback(&self) -> &'static str {
        match self {
            Lang::Ko => "현재 프로젝트",
            Lang::En => "current project",
        }
    }

    /// 권한 저장 중 — 재시도 없음 (초회 시도)
    pub fn msg_processing_no_retry(&self) -> &'static str {
        match self {
            Lang::Ko => "⏳ 권한 저장 중...",
            Lang::En => "⏳ Saving permission...",
        }
    }

    /// 권한 저장 중 — 재시도 횟수 포함
    pub fn msg_processing_with_attempt(&self, attempt: u32, total: u32) -> String {
        match self {
            Lang::Ko => format!("⏳ 권한 저장 중... (재시도 {attempt}/{total})"),
            Lang::En => format!("⏳ Saving permission... (retry {attempt}/{total})"),
        }
    }

    /// Always Allow 실패 — 최대 재시도 초과 (자동 거부)
    pub fn msg_save_failed_max_retries(&self, n: u32) -> String {
        match self {
            Lang::Ko => format!(
                "❌ {n}회 재시도 실패하여 자동으로 거부되었습니다. 다른 사용자가 settings 파일을 편집 중일 수 있어요. 잠시 후 같은 명령을 다시 실행해주세요."
            ),
            Lang::En => format!(
                "❌ Failed after {n} retries and was automatically denied. Another user may be editing the settings file. Please try the same command again later."
            ),
        }
    }

    pub fn msg_save_failed_lock_timeout(&self) -> &'static str {
        match self {
            Lang::Ko => "⏱ 권한 저장 실패 (lock timeout) — 다시 시도하세요",
            Lang::En => "⏱ Permission save failed (lock timeout) — please retry",
        }
    }

    pub fn msg_conflict_title(&self) -> &'static str {
        match self {
            Lang::Ko => "권한 충돌 알림",
            Lang::En => "Permission conflict",
        }
    }

    /// Bash input 이 너무 길어 truncate 된 경우 끝에 붙는 안내
    pub fn msg_input_truncated(&self) -> &'static str {
        match self {
            Lang::Ko => "... (입력이 길어 잘림)",
            Lang::En => "... (input truncated)",
        }
    }

    /// 권한 요청 메시지 전송 실패 시 채널에 표시하는 안내
    pub fn msg_send_failed_too_long(&self) -> &'static str {
        match self {
            Lang::Ko => "⚠️ 입력이 너무 길어 권한 요청 메시지를 표시할 수 없어 자동으로 거부되었어요. 짧은 명령으로 나눠서 시도해주세요.",
            Lang::En => "⚠️ Input too long to display permission request — automatically denied. Try splitting into shorter commands.",
        }
    }

    /// 보호 path 권한 prompt 안내 메시지 (5 prefix 매칭 대상)
    pub fn permission_protected_path_note(&self) -> &'static str {
        match self {
            Lang::Ko => "ℹ️ 이 파일은 Claude 보안 정책상 영속 매칭 불가 — 매번 prompt 발생",
            Lang::En => "ℹ️ This file cannot be persistently allowed by Claude policy — prompt every time",
        }
    }

    /// cwd 외부 파일 권한 prompt 안내 메시지
    pub fn permission_outside_cwd_note(&self) -> &'static str {
        match self {
            Lang::Ko => "ℹ️ 이 파일은 작업 디렉토리 외부 — 영속 매칭 불가 (`additionalDirectories` 설정 필요)",
            Lang::En => "ℹ️ This file is outside the working directory — persistent allow disabled (add to `additionalDirectories`)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btn_always_tool_has_warning_icon() {
        assert!(Lang::Ko.btn_always_tool().contains('⚠'), "Ko label must contain ⚠");
        assert!(Lang::En.btn_always_tool().contains('⚠'), "En label must contain ⚠");
        assert!(Lang::Ko.btn_always_tool().contains("도구 전체"), "Ko label must contain '도구 전체'");
        assert!(Lang::En.btn_always_tool().contains("Entire tool"), "En label must contain 'Entire tool'");
    }

    #[test]
    fn lbl_permission_request_section_header_format() {
        assert_eq!(Lang::Ko.lbl_permission_request_section_header(), "### 권한 요청");
        assert_eq!(Lang::En.lbl_permission_request_section_header(), "### Permission Request");
        assert!(Lang::Ko.lbl_permission_request_section_header().starts_with("###"));
        assert!(Lang::En.lbl_permission_request_section_header().starts_with("###"));
    }

    #[test]
    fn btn_scope_status_project_label() {
        assert_eq!(Lang::Ko.btn_scope_status_project(), "📁 적용 범위: 프로젝트");
        assert_eq!(Lang::En.btn_scope_status_project(), "📁 Applied to: project");
    }

    #[test]
    fn btn_scope_status_global_label() {
        assert_eq!(Lang::Ko.btn_scope_status_global(), "⚠️ 적용 범위: 전역");
        assert_eq!(Lang::En.btn_scope_status_global(), "⚠️ Applied to: global");
    }

    #[test]
    fn lang_msg_save_success_project() {
        let ko = Lang::Ko.msg_save_success_project("pidory", "Bash(npm install)");
        assert_eq!(ko, "✅ pidory에서 항상 허용됨: Bash(npm install)");

        let en = Lang::En.msg_save_success_project("pidory", "Bash(npm install)");
        assert_eq!(en, "✅ Always allowed in pidory: Bash(npm install)");
    }

    #[test]
    fn lang_msg_save_success_global() {
        let ko = Lang::Ko.msg_save_success_global("Bash(npm *)");
        assert_eq!(ko, "✅ 모든 프로젝트에서 항상 허용됨: Bash(npm *)");

        let en = Lang::En.msg_save_success_global("Bash(npm *)");
        assert_eq!(en, "✅ Always allowed in all projects: Bash(npm *)");
    }

    #[test]
    fn lang_msg_save_failed_max_retries() {
        let ko = Lang::Ko.msg_save_failed_max_retries(3);
        assert_eq!(
            ko,
            "❌ 3회 재시도 실패하여 자동으로 거부되었습니다. 다른 사용자가 settings 파일을 편집 중일 수 있어요. 잠시 후 같은 명령을 다시 실행해주세요."
        );

        let en = Lang::En.msg_save_failed_max_retries(3);
        assert!(en.contains("3 retries"));
        assert!(en.contains("automatically denied"));
    }

    #[test]
    fn lang_msg_processing_with_attempt() {
        let ko = Lang::Ko.msg_processing_with_attempt(2, 3);
        assert_eq!(ko, "⏳ 권한 저장 중... (재시도 2/3)");

        let en = Lang::En.msg_processing_with_attempt(2, 3);
        assert_eq!(en, "⏳ Saving permission... (retry 2/3)");
    }

    #[test]
    fn lang_msg_processing_no_retry() {
        assert_eq!(Lang::Ko.msg_processing_no_retry(), "⏳ 권한 저장 중...");
        assert_eq!(Lang::En.msg_processing_no_retry(), "⏳ Saving permission...");
    }

    #[test]
    fn lang_msg_project_basename_fallback() {
        assert_eq!(Lang::Ko.msg_project_basename_fallback(), "현재 프로젝트");
        assert_eq!(Lang::En.msg_project_basename_fallback(), "current project");
    }

    #[test]
    fn lang_msg_always_allow_options_header_is_bold() {
        assert!(Lang::Ko.msg_always_allow_options_header().starts_with("**"), "Ko must start with **");
        assert!(Lang::En.msg_always_allow_options_header().starts_with("**"), "En must start with **");
        assert!(Lang::Ko.msg_always_allow_options_header().ends_with("**"), "Ko must end with **");
        assert!(Lang::En.msg_always_allow_options_header().ends_with("**"), "En must end with **");
        assert!(!Lang::Ko.msg_always_allow_options_header().starts_with("-#"), "Ko must not start with -#");
        assert!(!Lang::En.msg_always_allow_options_header().starts_with("-#"), "En must not start with -#");
    }
}
