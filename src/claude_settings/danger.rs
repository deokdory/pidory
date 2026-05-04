//! Command danger classification types.
//!
//! # 책임 범위
//!
//! - [`Severity`] — permission rule의 위험도 등급
//! - [`classify_command`] — rule 문자열 → Severity 분류 (skeleton, P1.5 구현)

/// Permission rule의 위험도 등급.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    /// 안전한 명령 (읽기 전용, 부작용 없음)
    Safe,
    /// 주의가 필요한 명령 (상태 변경, 네트워크 접근 등)
    Moderate,
    /// 위험한 명령 (시스템 변경, 파일 삭제 등)
    Dangerous,
}

/// rule 문자열로 위험도를 분류한다.
///
/// **skeleton — 항상 `Severity::Safe` 반환.**
/// P1.5가 실제 분류 로직 구현.
pub fn classify_command(_rule: &str) -> Severity {
    // P1.5가 실제 분류 로직 구현
    Severity::Safe
}
