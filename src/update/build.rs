use std::process::Stdio;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::Error;

pub async fn build_release(
    worktree: &std::path::Path,
    on_progress: impl Fn(&str) + Send,
) -> Result<std::time::Duration, Error> {
    let start = Instant::now();

    let mut child = Command::new("cargo")
        .current_dir(worktree)
        .args(["build", "--release"])
        .env("CARGO_INCREMENTAL", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| Error::BuildFailed {
            stderr_tail: e.to_string(),
        })?;

    let stderr = child.stderr.take().ok_or_else(|| Error::BuildFailed {
        stderr_tail: "stderr unavailable".into(),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| Error::BuildFailed {
        stderr_tail: "stdout unavailable".into(),
    })?;

    let mut stderr_lines = BufReader::new(stderr).lines();
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_accum: Vec<String> = Vec::new();

    // stderr와 stdout을 동시에 소진해야 데드락 없음.
    // 각 스트림이 EOF에 도달하면 해당 플래그를 세우고 나머지 스트림을 계속 소진.
    let mut stderr_done = false;
    let mut stdout_done = false;

    while !stderr_done || !stdout_done {
        tokio::select! {
            biased;
            line = stderr_lines.next_line(), if !stderr_done => {
                match line {
                    Ok(Some(l)) => {
                        on_progress(&l);
                        stderr_accum.push(l);
                    }
                    _ => stderr_done = true,
                }
            }
            line = stdout_lines.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(_)) => {} // drain, 내용 불필요
                    _ => stdout_done = true,
                }
            }
        }
    }

    let status = child.wait().await.map_err(|e| Error::BuildFailed {
        stderr_tail: e.to_string(),
    })?;

    if !status.success() {
        let tail_start = stderr_accum.len().saturating_sub(50);
        let tail = stderr_accum[tail_start..].join("\n");
        return Err(Error::BuildFailed { stderr_tail: tail });
    }

    Ok(start.elapsed())
}
