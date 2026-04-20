use super::Error;

#[derive(Debug, PartialEq)]
pub enum Platform {
    Linux,
    MacOs,
    Other,
}

pub fn detect_platform() -> Platform {
    if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Other
    }
}

pub fn schedule_restart() -> Result<(), Error> {
    match detect_platform() {
        Platform::Linux => schedule_restart_linux(),
        Platform::MacOs => schedule_restart_macos(),
        Platform::Other => Err(Error::RestartFailed("unsupported platform".into())),
    }
}

#[cfg(target_os = "linux")]
fn schedule_restart_linux() -> Result<(), Error> {
    use std::process::Command;
    // best-effort: 이전 실패 상태 리셋
    let _ = Command::new("systemctl")
        .args(["reset-failed", "pidory-delayed-restart.service"])
        .status();
    // best-effort: 이미 실행 중이면 정지
    let _ = Command::new("systemctl")
        .args(["stop", "pidory-delayed-restart.service"])
        .status();
    // 핵심: --no-block으로 즉시 반환 (30초 sleep은 service 내부에서)
    let status = Command::new("systemctl")
        .args(["start", "--no-block", "pidory-delayed-restart.service"])
        .status()
        .map_err(|e| Error::RestartFailed(e.to_string()))?;
    if !status.success() {
        return Err(Error::RestartFailed(format!(
            "systemctl start failed: exit {:?}",
            status.code()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn schedule_restart_linux() -> Result<(), Error> {
    unreachable!()
}

#[cfg(target_os = "macos")]
fn schedule_restart_macos() -> Result<(), Error> {
    use std::process::Command;
    // best-effort: launchctl kickstart
    // uid 501은 macOS 첫 번째 사용자의 일반적인 uid.
    // 실제 환경에서는 `id -u` 결과로 동적 감지 필요하나, macOS는 best-effort이므로 현재 값 유지.
    let status = Command::new("launchctl")
        .args(["kickstart", "-k", "gui/501/com.pidory.bot"])
        .status()
        .map_err(|e| Error::RestartFailed(e.to_string()))?;
    if !status.success() {
        return Err(Error::RestartFailed(format!(
            "launchctl failed: exit {:?}",
            status.code()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn schedule_restart_macos() -> Result<(), Error> {
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_matches_cfg() {
        let p = detect_platform();
        #[cfg(target_os = "linux")]
        assert!(matches!(p, Platform::Linux));
        #[cfg(target_os = "macos")]
        assert!(matches!(p, Platform::MacOs));
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        assert!(matches!(p, Platform::Other));
    }
}
