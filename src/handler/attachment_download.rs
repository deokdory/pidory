use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::TryStreamExt as _;
use poise::serenity_prelude as serenity;
use tokio::io::AsyncWriteExt as _;

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_FILENAME_BYTES: usize = 200;

// ── DownloadError ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DownloadError {
    TooLarge { filename: String, size: u64, limit: u64 },
    AggregateLimit { total: u64, limit: u64 },
    NetworkError { filename: String, source: reqwest::Error },
    IoError { filename: String, source: std::io::Error },
    InvalidUrl { filename: String, url: String },
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::TooLarge { filename, size, limit } => write!(
                f,
                "파일이 너무 큽니다: {} ({:.1} MB > {} MB)",
                filename,
                *size as f64 / 1_048_576.0,
                *limit / (1024 * 1024)
            ),
            DownloadError::AggregateLimit { total, limit } => write!(
                f,
                "첨부파일 총 크기가 너무 큽니다: {:.1} MB > {} MB",
                *total as f64 / 1_048_576.0,
                *limit / (1024 * 1024)
            ),
            DownloadError::NetworkError { filename, source } => {
                write!(f, "네트워크 오류 ({}): {}", filename, source)
            }
            DownloadError::IoError { filename, source } => {
                write!(f, "파일 저장 오류 ({}): {}", filename, source)
            }
            DownloadError::InvalidUrl { filename, url } => {
                write!(f, "허용되지 않는 URL ({}): {}", filename, url)
            }
        }
    }
}

impl std::error::Error for DownloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DownloadError::NetworkError { source, .. } => Some(source),
            DownloadError::IoError { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ── sanitize_filename ────────────────────────────────────────────────────────

/// Sanitizes a filename for safe use as a filesystem path component.
///
/// - Removes path separators (`/`, `\`), null bytes (`\0`), and `..` sequences
/// - Removes leading dots (dotfile prevention)
/// - Replaces spaces with `_`
/// - Truncates to `MAX_FILENAME_BYTES` bytes while preserving extension
/// - Returns `"unnamed"` if the result would be empty
pub fn sanitize_filename(filename: &str) -> String {
    // Remove path separators and control characters (includes null bytes)
    let s = filename
        .replace(['/', '\\'], "")
        .replace(|c: char| c.is_ascii_control(), "");

    // Remove `..` sequences
    let s = s.replace("..", "");

    // Replace spaces with underscores
    let s = s.replace(' ', "_");

    // Remove leading dots
    let s = s.trim_start_matches('.').to_owned();

    if s.is_empty() {
        return "unnamed".to_owned();
    }

    // Truncate to MAX_FILENAME_BYTES, preserving extension
    if s.len() <= MAX_FILENAME_BYTES {
        return s;
    }

    // Split stem and extension at last dot
    if let Some(dot_pos) = s.rfind('.') {
        let (stem, ext) = s.split_at(dot_pos);
        // ext includes the dot
        if ext.len() >= MAX_FILENAME_BYTES {
            // Extension itself is too long; just truncate the whole string
            truncate_to_bytes(&s, MAX_FILENAME_BYTES)
        } else {
            let max_stem = MAX_FILENAME_BYTES - ext.len();
            // Truncate stem at a byte boundary
            let truncated_stem = truncate_to_bytes(stem, max_stem);
            format!("{}{}", truncated_stem, ext)
        }
    } else {
        truncate_to_bytes(&s, MAX_FILENAME_BYTES)
    }
}

/// Truncates `s` to at most `max_bytes` bytes at a valid UTF-8 char boundary.
fn truncate_to_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

// ── validate_url ─────────────────────────────────────────────────────────────

/// Returns `true` only for Discord CDN/media URLs.
pub fn validate_url(url: &str) -> bool {
    url.starts_with("https://cdn.discordapp.com/")
        || url.starts_with("https://media.discordapp.net/")
}

// ── resolve_and_open ──────────────────────────────────────────────────────────

/// Inner implementation — `cap` controls the maximum n value in the suffix retry loop.
///
/// Opens the first available path atomically with `create_new(true)` and returns
/// the `(PathBuf, File)` pair. The caller writes the stream content directly into
/// the returned handle, eliminating the TOCTOU window that would exist if path
/// resolution and file creation were separate steps.
///
/// - First attempt: `{canonical_dir}/{message_id}_{sanitized}` (no suffix)
/// - On `AlreadyExists`: `{canonical_dir}/{message_id}_{stem}_{n}{ext}` for n=2..=cap
/// - Cap exhausted → `Err(io::ErrorKind::AlreadyExists)`
///
/// Separated from the public wrapper for testability (callers can pass a small cap).
async fn resolve_and_open_with_cap(
    canonical_dir: &Path,
    message_id: u64,
    sanitized: &str,
    cap: u32,
) -> Result<(PathBuf, tokio::fs::File), io::Error> {
    // First attempt: no suffix — {message_id}_{sanitized}
    let base_name = format!("{}_{}", message_id, sanitized);
    let first = canonical_dir.join(&base_name);

    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&first)
        .await
    {
        Ok(file) => return Ok((first, file)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // Fall through to retry loop
        }
        Err(e) => return Err(e),
    }

    // Decompose sanitized into stem + extension for suffix insertion
    let raw = Path::new(sanitized);
    let stem = raw
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = raw
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    for n in 2..=cap {
        // Pattern: {message_id}_{stem}_{n}{ext}
        let candidate_name = if stem.is_empty() {
            format!("{}__{}{}", message_id, n, ext)
        } else {
            format!("{}_{}_{}{}", message_id, stem, n, ext)
        };
        let candidate = canonical_dir.join(&candidate_name);

        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await
        {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("too many collisions for {}", base_name),
    ))
}

/// Opens the first available destination path atomically, returning the path and
/// an open `File` handle ready for writing.
///
/// - First attempt: `{canonical_dir}/{message_id}_{sanitized}` (no suffix)
/// - On collision: `{canonical_dir}/{message_id}_{stem}_{n}{ext}` for n=2..=999
/// - Cap: 999 total attempts (first + 998 retries). Returns `Err` if exhausted.
/// - Uses `create_new(true)` to atomically claim the slot — no TOCTOU window.
async fn resolve_and_open(
    canonical_dir: &Path,
    message_id: u64,
    sanitized: &str,
) -> Result<(PathBuf, tokio::fs::File), io::Error> {
    resolve_and_open_with_cap(canonical_dir, message_id, sanitized, 999).await
}

// ── download_attachments ─────────────────────────────────────────────────────

/// Downloads Discord attachments to `{project_path}/.pidory/downloads/{thread_id}/`.
///
/// Returns `(success_paths, errors)`. All attachments are attempted; failures
/// are collected without aborting the rest.
pub async fn download_attachments(
    attachments: &[serenity::Attachment],
    project_path: &Path,
    thread_id: u64,
    message_id: u64,
    max_file_size: u64,
    max_aggregate_size: u64,
    download_timeout_secs: u64,
) -> (Vec<String>, Vec<DownloadError>) {
    if attachments.is_empty() {
        return (vec![], vec![]);
    }

    let total: u64 = attachments.iter().map(|a| a.size as u64).sum();
    if total > max_aggregate_size {
        return (vec![], vec![DownloadError::AggregateLimit { total, limit: max_aggregate_size }]);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(download_timeout_secs))
        .build()
        .unwrap_or_default();

    let download_dir = project_path
        .join(".pidory")
        .join("downloads")
        .join(thread_id.to_string());

    let mut paths: Vec<String> = Vec::new();
    let mut errors: Vec<DownloadError> = Vec::new();

    // Create download dir once before the loop
    if let Err(e) = tokio::fs::create_dir_all(&download_dir).await {
        return (
            vec![],
            attachments
                .iter()
                .map(|a| DownloadError::IoError {
                    filename: a.filename.clone(),
                    source: std::io::Error::new(e.kind(), e.to_string()),
                })
                .collect(),
        );
    }

    // Canonicalize before the loop to defend against TOCTOU symlink races
    let canonical_dir = match tokio::fs::canonicalize(&download_dir).await {
        Ok(p) => p,
        Err(e) => {
            return (
                vec![],
                attachments
                    .iter()
                    .map(|a| DownloadError::IoError {
                        filename: a.filename.clone(),
                        source: std::io::Error::new(e.kind(), e.to_string()),
                    })
                    .collect(),
            );
        }
    };

    for attachment in attachments {
        let filename = attachment.filename.clone();
        let size = attachment.size as u64;

        tracing::debug!("downloading attachment: {} ({} bytes)", filename, size);

        if size > max_file_size {
            errors.push(DownloadError::TooLarge {
                filename,
                size,
                limit: max_file_size,
            });
            continue;
        }

        if !validate_url(&attachment.url) {
            errors.push(DownloadError::InvalidUrl {
                filename,
                url: attachment.url.clone(),
            });
            continue;
        }

        let sanitized = sanitize_filename(&filename);

        // HTTP request first — only claim a slot after success is confirmed.
        let resp = match client.get(&attachment.url).send().await {
            Ok(r) => match r.error_for_status() {
                Ok(r) => r,
                Err(e) => {
                    errors.push(DownloadError::NetworkError { filename, source: e });
                    continue;
                }
            },
            Err(e) => {
                errors.push(DownloadError::NetworkError { filename, source: e });
                continue;
            }
        };

        // HTTP success confirmed — now atomically claim the slot.
        let (dest_path, dest_file) =
            match resolve_and_open(&canonical_dir, message_id, &sanitized).await {
                Ok(pair) => pair,
                Err(e) => {
                    errors.push(DownloadError::IoError { filename, source: e });
                    continue;
                }
            };

        // dest_file was opened with create_new(true) in resolve_and_open — no TOCTOU window.
        match write_stream_to_file(resp.bytes_stream(), dest_file, &dest_path, max_file_size).await {
            Ok(WriteStreamOutcome::Done { .. }) => {}
            Ok(WriteStreamOutcome::TooLarge { written }) => {
                errors.push(DownloadError::TooLarge {
                    filename,
                    size: written,
                    limit: max_file_size,
                });
                continue;
            }
            Ok(WriteStreamOutcome::NetworkError { source }) => {
                errors.push(DownloadError::NetworkError { filename, source });
                continue;
            }
            Err(e) => {
                errors.push(DownloadError::IoError { filename, source: e });
                continue;
            }
        }

        // Path traversal final defense: canonicalize and verify prefix
        let canonical = match tokio::fs::canonicalize(&dest_path).await {
            Ok(p) => p,
            Err(e) => {
                let _ = tokio::fs::remove_file(&dest_path).await;
                errors.push(DownloadError::IoError {
                    filename,
                    source: e,
                });
                continue;
            }
        };

        if !canonical.starts_with(&canonical_dir) {
            let _ = tokio::fs::remove_file(&dest_path).await;
            errors.push(DownloadError::IoError {
                filename,
                source: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "path traversal detected",
                ),
            });
            continue;
        }

        tracing::debug!("downloaded attachment: {} -> {}", filename, dest_path.display());
        paths.push(canonical.to_string_lossy().into_owned());
    }

    (paths, errors)
}

// ── write_stream_to_file ─────────────────────────────────────────────────────

#[allow(dead_code)]
enum WriteStreamOutcome {
    Done { written: u64 },
    TooLarge { written: u64 },
    NetworkError { source: reqwest::Error },
}

/// Writes `stream` into the already-open `file` handle.
///
/// The caller is responsible for opening `file` with `create_new(true)` before
/// calling this function — no additional open is performed here.
///
/// `dest_path` is retained for cleanup (`remove_file`) on failure. It must match
/// the path that was used to open `file`.
///
/// Returns `WriteStreamOutcome::TooLarge` and deletes the partial file if the
/// total bytes written exceeds `max_file_size`. Returns `NetworkError` (and
/// deletes) on stream error. Returns `Err` on I/O write failure.
async fn write_stream_to_file<S>(
    stream: S,
    file: tokio::fs::File,
    dest_path: &std::path::Path,
    max_file_size: u64,
) -> Result<WriteStreamOutcome, std::io::Error>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    let mut file = file;
    let mut stream = stream;
    let mut written: u64 = 0;

    loop {
        match stream.try_next().await {
            Ok(Some(chunk)) => {
                written += chunk.len() as u64;
                if written > max_file_size {
                    drop(file);
                    let _ = tokio::fs::remove_file(dest_path).await;
                    return Ok(WriteStreamOutcome::TooLarge { written });
                }
                file.write_all(&chunk).await?;
            }
            Ok(None) => break,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(dest_path).await;
                return Ok(WriteStreamOutcome::NetworkError { source: e });
            }
        }
    }
    file.flush().await?;
    Ok(WriteStreamOutcome::Done { written })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_path_traversal() {
        let result = sanitize_filename("../../etc/passwd");
        // Path separators and `..` should be removed
        assert!(!result.contains('/'));
        assert!(!result.contains(".."));
    }

    #[test]
    fn sanitize_null_byte() {
        let result = sanitize_filename("foo\0bar.txt");
        assert_eq!(result, "foobar.txt");
    }

    #[test]
    fn sanitize_leading_dot() {
        let result = sanitize_filename(".env");
        assert_eq!(result, "env");
    }

    #[test]
    fn sanitize_long_filename() {
        // 296 'a' chars + ".txt" = 300 bytes total
        let stem = "a".repeat(296);
        let long_name = format!("{}.txt", stem);
        assert_eq!(long_name.len(), 300);

        let result = sanitize_filename(&long_name);
        assert!(result.len() <= MAX_FILENAME_BYTES);
        assert!(result.ends_with(".txt"));
    }

    #[test]
    fn sanitize_empty() {
        let result = sanitize_filename("");
        assert_eq!(result, "unnamed");
    }

    #[test]
    fn sanitize_normal() {
        let result = sanitize_filename("screenshot.png");
        assert_eq!(result, "screenshot.png");
    }

    #[test]
    fn sanitize_spaces() {
        let result = sanitize_filename("my file.txt");
        assert_eq!(result, "my_file.txt");
    }

    #[test]
    fn sanitize_backslash() {
        let result = sanitize_filename("path\\to\\file.txt");
        assert!(!result.contains('\\'));
    }

    #[test]
    fn sanitize_control_chars() {
        let result = sanitize_filename("hello\x01\x7Fworld.txt");
        assert_eq!(result, "helloworld.txt");
    }

    #[test]
    fn validate_url_discord_cdn() {
        assert!(validate_url(
            "https://cdn.discordapp.com/attachments/123/456/file.png"
        ));
    }

    #[test]
    fn validate_url_media() {
        assert!(validate_url(
            "https://media.discordapp.net/attachments/123/456/file.png"
        ));
    }

    #[test]
    fn validate_url_evil() {
        assert!(!validate_url("https://evil.com/malware"));
    }

    #[test]
    fn validate_url_no_scheme() {
        assert!(!validate_url("cdn.discordapp.com/file"));
    }

    #[tokio::test]
    async fn download_empty_attachments() {
        let (paths, errors) =
            download_attachments(&[], Path::new("/tmp"), 0, 0, 500 * 1024 * 1024, 500 * 1024 * 1024, 120).await;
        assert!(paths.is_empty());
        assert!(errors.is_empty());
    }

    #[tokio::test]
    async fn aggregate_size_over_limit() {
        use ::serenity::model::channel::Attachment;

        // 30 MB each × 2 = 60 MB > 50 MB aggregate limit
        let make = |id: u64, name: &str| -> Attachment {
            serde_json::from_value(serde_json::json!({
                "id": id.to_string(),
                "filename": name,
                "size": 31_457_280u64,
                "url": format!("https://cdn.discordapp.com/attachments/1/{}/{}", id, name),
                "proxy_url": format!("https://media.discordapp.net/attachments/1/{}/{}", id, name),
            }))
            .expect("attachment json")
        };

        let attachments = vec![make(1, "file1.txt"), make(2, "file2.txt")];
        let (paths, errors) = download_attachments(
            &attachments,
            Path::new("/tmp"),
            0,
            0,
            500 * 1024 * 1024,
            52_428_800,
            120,
        )
        .await;
        assert!(paths.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], DownloadError::AggregateLimit { .. }));
    }

    #[tokio::test]
    async fn stream_write_too_large() {
        use bytes::Bytes;
        use futures_util::stream;

        let dir = tempfile::tempdir().unwrap();
        let (path, file) = resolve_and_open_with_cap(dir.path(), 1, "test.bin", 5)
            .await
            .unwrap();

        // 3 chunks of 100 bytes each, limit = 200 bytes
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from(vec![0u8; 100])),
            Ok(Bytes::from(vec![0u8; 100])),
            Ok(Bytes::from(vec![0u8; 100])),
        ];
        let mock_stream = stream::iter(chunks);

        let outcome = write_stream_to_file(mock_stream, file, &path, 200).await.unwrap();
        assert!(matches!(outcome, WriteStreamOutcome::TooLarge { .. }));
        assert!(!path.exists(), "partial file should be deleted");
    }

    #[tokio::test]
    async fn stream_write_within_limit() {
        use bytes::Bytes;
        use futures_util::stream;

        let dir = tempfile::tempdir().unwrap();
        let (path, file) = resolve_and_open_with_cap(dir.path(), 2, "test.bin", 5)
            .await
            .unwrap();

        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from(vec![0u8; 100])),
            Ok(Bytes::from(vec![0u8; 100])),
        ];
        let mock_stream = stream::iter(chunks);

        let outcome = write_stream_to_file(mock_stream, file, &path, 500).await.unwrap();
        assert!(matches!(outcome, WriteStreamOutcome::Done { written: 200 }));
        assert!(path.exists(), "file should exist");
        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── resolve_and_open tests ────────────────────────────────────────────────

    /// Case 1: no collision → returns (path, File) with original name {msg_id}_image.png
    #[tokio::test]
    async fn collision_free_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (result, _file) = resolve_and_open(dir.path(), 42, "image.png")
            .await
            .unwrap();
        assert_eq!(
            result.file_name().unwrap().to_string_lossy(),
            "42_image.png"
        );
        // File handle keeps the file alive — it should exist
        assert!(result.exists(), "file should exist while handle is held");
    }

    /// Case 2: _2 already exists → returns _3 path + File handle
    #[tokio::test]
    async fn collision_free_skip_to_3() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        // Pre-create: 42_image.png and 42_image_2.png
        std::fs::write(path.join("42_image.png"), b"").unwrap();
        std::fs::write(path.join("42_image_2.png"), b"").unwrap();

        // cap=5 is more than enough
        let (result, _file) = resolve_and_open_with_cap(path, 42, "image.png", 5)
            .await
            .unwrap();
        assert_eq!(
            result.file_name().unwrap().to_string_lossy(),
            "42_image_3.png"
        );
        assert!(result.exists(), "file should exist while handle is held");
    }

    /// Case 3: cap exhausted → Err (use cap=3 so we only need 3 dummy files)
    #[tokio::test]
    async fn collision_free_cap_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        // Pre-create: base + _2 + _3 (cap=3 means n goes 2..=3, first attempt + 2 retries = 3 total)
        std::fs::write(path.join("1_image.png"), b"").unwrap();
        std::fs::write(path.join("1_image_2.png"), b"").unwrap();
        std::fs::write(path.join("1_image_3.png"), b"").unwrap();

        let result = resolve_and_open_with_cap(path, 1, "image.png", 3).await;
        assert!(result.is_err(), "should return Err when cap is exhausted");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("too many collisions"), "error message: {}", msg);
    }

    /// Case 4: no extension (e.g. README) → collision yields README_2
    #[tokio::test]
    async fn collision_free_no_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        // Pre-create base
        std::fs::write(path.join("7_README"), b"").unwrap();

        let (result, _file) = resolve_and_open_with_cap(path, 7, "README", 5)
            .await
            .unwrap();
        assert_eq!(
            result.file_name().unwrap().to_string_lossy(),
            "7_README_2"
        );
        assert!(result.exists(), "file should exist while handle is held");
    }

    /// Case 5: empty sanitized name → pattern still works (base = "{msg_id}_")
    #[tokio::test]
    async fn collision_free_empty_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        // No pre-existing files — should succeed on first attempt
        let (result, _file) = resolve_and_open_with_cap(dir.path(), 99, "", 5)
            .await
            .unwrap();
        // First attempt filename is "99_"
        assert_eq!(
            result.file_name().unwrap().to_string_lossy(),
            "99_"
        );
        assert!(result.exists(), "file should exist while handle is held");
    }
}
