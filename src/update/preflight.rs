//! /update 진입 직전 PostgreSQL 셋업 사전 검증. 미완 시 PreflightReport.missing 리스트 반환.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use crate::i18n::Lang;

#[derive(Debug, Clone, PartialEq)]
pub enum MissingItem {
    DatabaseUrl,
    ExecStartPre,
    MigrateBinary,
    PostgresConnection { reason: String },
}

#[derive(Debug, Clone)]
pub struct PreflightReport {
    missing: Vec<MissingItem>,
}

impl PreflightReport {
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    #[allow(dead_code)]
    pub fn missing(&self) -> &[MissingItem] {
        &self.missing
    }

    pub fn missing_labels(&self, lang: Lang) -> Vec<String> {
        self.missing
            .iter()
            .map(|item| match item {
                MissingItem::DatabaseUrl => lang.preflight_label_database_url().to_string(),
                MissingItem::ExecStartPre => lang.preflight_label_exec_start_pre().to_string(),
                MissingItem::MigrateBinary => lang.preflight_label_migrate_binary().to_string(),
                MissingItem::PostgresConnection { reason } => {
                    format!("{} ({})", lang.preflight_label_postgres_connection(), reason)
                }
            })
            .collect()
    }
}

fn check_database_url() -> bool {
    if std::env::var("DATABASE_URL").is_ok() {
        return true;
    }
    std::fs::read_to_string("/etc/pidory/db.env")
        .map(|s| s.lines().any(|l| l.trim_start().starts_with("DATABASE_URL=")))
        .unwrap_or(false)
}

static EXEC_START_PRE_RE: OnceLock<regex::Regex> = OnceLock::new();

pub(crate) fn check_exec_start_pre_in_text(text: &str) -> bool {
    let re = EXEC_START_PRE_RE.get_or_init(|| {
        // systemd prefix: '-' (ignore failure), '+' (full privs), '!' (skip security checks),
        // '@' (override argv[0]), '|' (pipe), ':' (no env expansion)
        regex::Regex::new(r"(?m)^ExecStartPre=-?[+!@|:]*/usr/local/bin/pidory-migrate(\s|$)")
            .expect("static regex literal")
    });
    re.is_match(text)
}

async fn check_exec_start_pre() -> bool {
    // 1차: systemctl cat (spawn_blocking + 3s timeout)
    let systemctl_result = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("systemctl")
                .args(["cat", "pidory.service"])
                .output()
        }),
    )
    .await;

    if let Ok(Ok(Ok(output))) = systemctl_result {
        if let Ok(text) = String::from_utf8(output.stdout) {
            if check_exec_start_pre_in_text(&text) {
                return true;
            }
        }
    }

    // 2차 fallback: 직접 파일 읽기 (spawn_blocking + 1s timeout)
    let fs_result = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::task::spawn_blocking(|| {
            std::fs::read_to_string("/etc/systemd/system/pidory.service")
        }),
    )
    .await;

    if let Ok(Ok(Ok(text))) = fs_result {
        if check_exec_start_pre_in_text(&text) {
            return true;
        }
    }

    false
}

#[cfg(unix)]
pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
pub(crate) fn is_executable(_path: &Path) -> bool {
    false // 비-Unix 빌드는 셋업 미완으로 간주 (안전 default)
}

fn check_migrate_binary() -> bool {
    let path = Path::new("/usr/local/bin/pidory-migrate");
    path.exists() && is_executable(path)
}

async fn check_postgres_connection() -> Option<MissingItem> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => return None, // DATABASE_URL 없으면 skip (item 1에서 이미 보고)
    };

    use sqlx::Connection;

    match tokio::time::timeout(
        Duration::from_secs(5),
        sqlx::PgConnection::connect(&url),
    )
    .await
    {
        Err(_) => Some(MissingItem::PostgresConnection {
            reason: "connection timed out".to_string(),
        }),
        Ok(Err(_)) => Some(MissingItem::PostgresConnection {
            reason: "connect failed".to_string(),
        }),
        Ok(Ok(mut conn)) => {
            if sqlx::Executor::execute(&mut conn, "SELECT 1")
                .await
                .is_err()
            {
                Some(MissingItem::PostgresConnection {
                    reason: "select failed".to_string(),
                })
            } else {
                None
            }
        }
    }
}

pub async fn check_postgres_setup() -> PreflightReport {
    #[cfg(not(target_os = "linux"))]
    {
        return PreflightReport { missing: vec![] };
    }

    #[cfg(target_os = "linux")]
    {
        let mut missing = Vec::new();

        if !check_database_url() {
            missing.push(MissingItem::DatabaseUrl);
        }

        if !check_exec_start_pre().await {
            missing.push(MissingItem::ExecStartPre);
        }

        if !check_migrate_binary() {
            missing.push(MissingItem::MigrateBinary);
        }

        if let Some(item) = check_postgres_connection().await {
            missing.push(item);
        }

        PreflightReport { missing }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn check_exec_start_pre_in_text_matches_valid_unit() {
        let text =
            "[Service]\nExecStartPre=/usr/local/bin/pidory-migrate\nExecStart=/usr/local/bin/pidory";
        assert!(check_exec_start_pre_in_text(text));
    }

    #[test]
    fn check_exec_start_pre_in_text_rejects_missing() {
        let text = "[Service]\nExecStart=/usr/local/bin/pidory";
        assert!(!check_exec_start_pre_in_text(text));
    }

    #[test]
    fn check_exec_start_pre_in_text_rejects_wrong_binary() {
        let text = "[Service]\nExecStartPre=/bin/echo hello";
        assert!(!check_exec_start_pre_in_text(text));
    }

    #[test]
    fn check_exec_start_pre_in_text_accepts_with_args() {
        let text = "ExecStartPre=/usr/local/bin/pidory-migrate --foo";
        assert!(check_exec_start_pre_in_text(text));
    }

    #[test]
    fn preflight_report_is_complete_when_empty() {
        let report = PreflightReport { missing: vec![] };
        assert!(report.is_complete());
    }

    #[test]
    fn preflight_report_lists_all_missing() {
        let report = PreflightReport {
            missing: vec![
                MissingItem::DatabaseUrl,
                MissingItem::ExecStartPre,
                MissingItem::MigrateBinary,
            ],
        };
        assert!(!report.is_complete());
        assert_eq!(report.missing().len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn migrate_binary_executable_detection() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake");

        fs::write(&fake, b"").unwrap();

        // 0o755 → executable
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&fake));

        // 0o644 → not executable
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&fake));
    }

    #[test]
    fn check_exec_start_pre_in_text_rejects_wrapper_with_substring() {
        // echo 가 실제 실행 바이너리 — full path 매칭 강화로 거부
        let text = "ExecStartPre=/bin/echo /usr/local/bin/pidory-migrate";
        assert!(!check_exec_start_pre_in_text(text));
    }

    #[test]
    fn check_exec_start_pre_in_text_accepts_systemd_prefix() {
        // '-' prefix = 실패 무시 systemd 문법
        let text = "ExecStartPre=-/usr/local/bin/pidory-migrate";
        assert!(check_exec_start_pre_in_text(text));
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_returns_false_on_missing() {
        let path = Path::new("/tmp/nonexistent-foobar-xyz-pidory-test-abc123");
        assert!(!is_executable(path));
    }
}
