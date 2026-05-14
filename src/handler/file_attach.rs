use std::fmt;
use std::path::Path;

use poise::serenity_prelude as serenity;
use tracing::warn;

use crate::error::PidoryError;
use crate::i18n::Lang;

// ── Marker parsing ──────────────────────────────────────────────────────────

const MARKER_PREFIX: &str = "<!--pidory:attach:";
const MARKER_SUFFIX: &str = "-->";

/// Extracts `<!--pidory:attach:{path}-->` markers from `text`.
///
/// Returns `(cleaned_text, vec_of_paths)`. All markers are removed from the
/// returned text; surrounding whitespace is collapsed but otherwise left
/// intact.
///
/// Markers inside code fences (``` ... ```) or inline backticks (` ... `) are
/// skipped — they remain in the text as-is and no path is extracted.
pub fn extract_file_markers(text: &str) -> (String, Vec<String>) {
    let mut paths: Vec<String> = Vec::new();
    let mut cleaned = String::with_capacity(text.len());

    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_fence = false;
    let mut in_inline = false;

    while i < len {
        if in_fence {
            // Only a ``` triple-tick closes the fence
            if bytes[i..].starts_with(b"```") {
                in_fence = false;
                cleaned.push_str("```");
                i += 3;
            } else {
                let ch = text[i..].chars().next().expect("byte index aligns to char boundary");
                cleaned.push(ch);
                i += ch.len_utf8();
            }
        } else if in_inline {
            // A single ` closes the inline span
            if bytes[i] == b'`' {
                in_inline = false;
                cleaned.push('`');
                i += 1;
            } else {
                let ch = text[i..].chars().next().expect("byte index aligns to char boundary");
                cleaned.push(ch);
                i += ch.len_utf8();
            }
        } else {
            // Outside fence and inline
            if bytes[i..].starts_with(b"```") {
                in_fence = true;
                cleaned.push_str("```");
                i += 3;
            } else if bytes[i] == b'`' {
                in_inline = true;
                cleaned.push('`');
                i += 1;
            } else if bytes[i..].starts_with(MARKER_PREFIX.as_bytes()) {
                let after_prefix = i + MARKER_PREFIX.len();
                if let Some(rel) = bytes[after_prefix..].windows(MARKER_SUFFIX.len()).position(|w| w == MARKER_SUFFIX.as_bytes()) {
                    let path = &text[after_prefix..after_prefix + rel];
                    if !path.is_empty() {
                        paths.push(path.to_owned());
                    }
                    i = after_prefix + rel + MARKER_SUFFIX.len();
                } else {
                    // Malformed marker — keep as-is and stop searching
                    cleaned.push_str(&text[i..]);
                    i = len;
                }
            } else {
                let ch = text[i..].chars().next().expect("byte index aligns to char boundary");
                cleaned.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    (cleaned, paths)
}

// ── FileAttachError ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum FileAttachError {
    NotFound(String),
    TooLarge { path: String, size: u64 },
    PermissionDenied(String),
    IoError(std::io::Error),
}

impl fmt::Display for FileAttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileAttachError::NotFound(p) => write!(f, "File not found: {}", p),
            FileAttachError::TooLarge { path, size } => write!(
                f,
                "File too large: {} ({} > 25 MB)",
                path,
                format_file_size(*size)
            ),
            FileAttachError::PermissionDenied(p) => {
                write!(f, "Permission denied: {}", p)
            }
            FileAttachError::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for FileAttachError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FileAttachError::IoError(e) => Some(e),
            _ => None,
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

const MAX_FILE_SIZE: u64 = 26_214_400; // 25 MiB

pub fn format_file_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

// ── Security helpers ─────────────────────────────────────────────────────────

/// Returns `true` if `path` refers to a sensitive location that must not be
/// served as a Discord attachment (dotfiles under HOME, system credential
/// files, virtual filesystems).
fn is_sensitive_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Block ~/.anything (dotfiles/dotdirs directly under HOME)
    if let Some(home) = std::env::var_os("HOME") {
        let home = Path::new(&home);
        if let Ok(relative) = path.strip_prefix(home)
            && let Some(std::path::Component::Normal(name)) = relative.components().next()
            && name.to_string_lossy().starts_with('.')
        {
            return true;
        }
    }

    // Block well-known system sensitive paths and virtual filesystems
    let blocked_prefixes = [
        "/etc/shadow",
        "/etc/passwd",
        "/etc/ssl/private",
        "/proc/",
        "/sys/",
        "/dev/",
    ];
    blocked_prefixes
        .iter()
        .any(|prefix| path_str.starts_with(prefix))
}

// ── prepare_attachment ───────────────────────────────────────────────────────

/// Validates the file at `path` and returns a ready-to-send `CreateAttachment`
/// along with the file size in bytes.
///
/// Errors:
/// - `NotFound`        — path does not exist or cannot be resolved
/// - `TooLarge`        — file exceeds 25 MiB
/// - `PermissionDenied`— OS permission error
/// - `IoError`         — any other IO failure
pub async fn prepare_attachment(
    path: &str,
) -> Result<(serenity::CreateAttachment, u64), FileAttachError> {
    // Resolve symlinks and verify existence
    let canonical = tokio::fs::canonicalize(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FileAttachError::NotFound(path.to_owned())
        } else if e.kind() == std::io::ErrorKind::PermissionDenied {
            FileAttachError::PermissionDenied(path.to_owned())
        } else {
            FileAttachError::IoError(e)
        }
    })?;

    // Block sensitive paths (dotfiles, system credentials, virtual filesystems)
    if is_sensitive_path(&canonical) {
        return Err(FileAttachError::PermissionDenied(path.to_owned()));
    }

    // Check file metadata
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            FileAttachError::PermissionDenied(path.to_owned())
        } else {
            FileAttachError::IoError(e)
        }
    })?;

    // Block non-regular files (directories, symlinks, FIFOs, /dev/zero, etc.)
    if !metadata.file_type().is_file() {
        return Err(FileAttachError::NotFound(path.to_owned()));
    }

    let size = metadata.len();
    if size > MAX_FILE_SIZE {
        return Err(FileAttachError::TooLarge {
            path: path.to_owned(),
            size,
        });
    }

    // Read file
    let data = tokio::fs::read(&canonical).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            FileAttachError::PermissionDenied(path.to_owned())
        } else {
            FileAttachError::IoError(e)
        }
    })?;

    let filename = Path::new(&canonical)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".to_owned());

    Ok((serenity::CreateAttachment::bytes(data, filename), size))
}

// ── send_file_attachments ────────────────────────────────────────────────────

/// Sends each path as a Discord file attachment to `channel_id`.
///
/// Processes all paths even if some fail. Errors are sent as text messages
/// to the channel. Returns `Err` only if a Discord API call itself fails.
pub async fn send_file_attachments(
    ctx: &serenity::Context,
    channel_id: serenity::ChannelId,
    paths: &[String],
    lang: Lang,
) -> Result<(), PidoryError> {
    for path in paths {
        match prepare_attachment(path).await {
            Ok((attachment, file_size)) => {
                let filename = Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());

                let size_str = format_file_size(file_size);
                let content = lang.file_attached(&filename, &size_str);
                let message = serenity::CreateMessage::new()
                    .content(content)
                    .add_file(attachment)
                    .allowed_mentions(serenity::CreateAllowedMentions::new());
                if let Err(e) = channel_id.send_message(ctx, message).await {
                    warn!("Failed to send file attachment for {}: {}", path, e);
                    let error_content = lang.file_send_failed(&filename, &size_str, &e.to_string());
                    let fallback = serenity::CreateMessage::new()
                        .content(error_content)
                        .allowed_mentions(serenity::CreateAllowedMentions::new());
                    if let Err(e2) = channel_id.send_message(ctx, fallback).await {
                        warn!("Failed to send fallback error message for {}: {}", path, e2);
                    }
                }
            }
            Err(e) => {
                let content = match &e {
                    FileAttachError::NotFound(p) => lang.file_not_found(p),
                    FileAttachError::TooLarge { path: p, size } => {
                        let name = Path::new(p)
                            .file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or(std::borrow::Cow::Borrowed(p.as_str()));
                        lang.file_too_large(name.as_ref(), *size as f64 / 1_048_576.0)
                    }
                    FileAttachError::PermissionDenied(p) => lang.file_permission_denied(p),
                    FileAttachError::IoError(_) => lang.file_attach_error(path, &e.to_string()),
                };
                let error_msg = serenity::CreateMessage::new()
                    .content(content)
                    .allowed_mentions(serenity::CreateAllowedMentions::new());
                if let Err(e) = channel_id.send_message(ctx, error_msg).await {
                    warn!("Failed to send error message for {}: {}", path, e);
                }
            }
        }
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_single_marker() {
        let input = "Hello <!--pidory:attach:/tmp/foo.txt--> World";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(text, "Hello  World");
        assert_eq!(paths, vec!["/tmp/foo.txt"]);
    }

    #[test]
    fn extract_multiple_markers() {
        let input =
            "A <!--pidory:attach:/a.txt--> B <!--pidory:attach:/b/c.rs--> C";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(text, "A  B  C");
        assert_eq!(paths, vec!["/a.txt", "/b/c.rs"]);
    }

    #[test]
    fn extract_no_markers() {
        let input = "No markers here.";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(text, "No markers here.");
        assert!(paths.is_empty());
    }

    #[test]
    fn extract_path_with_spaces_and_korean() {
        let input = "<!--pidory:attach:/home/user/내 파일.txt-->";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(text, "");
        assert_eq!(paths, vec!["/home/user/내 파일.txt"]);
    }

    #[test]
    fn extract_malformed_marker_kept() {
        let input = "<!--pidory:attach:/no-close";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(text, "<!--pidory:attach:/no-close");
        assert!(paths.is_empty());
    }

    #[test]
    fn format_file_size_bytes() {
        assert_eq!(format_file_size(512), "512 B");
    }

    #[test]
    fn format_file_size_kb() {
        assert_eq!(format_file_size(2048), "2.0 KB");
    }

    #[test]
    fn format_file_size_mb() {
        assert_eq!(format_file_size(5 * 1_048_576), "5.0 MB");
    }

    #[test]
    fn format_file_size_gb() {
        assert_eq!(format_file_size(2 * 1_073_741_824), "2.0 GB");
    }

    // ── extract_file_markers edge cases ─────────────────────────────────────

    #[test]
    fn extract_consecutive_markers_no_text_between() {
        let input = "<!--pidory:attach:/a--><!--pidory:attach:/b-->";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(text, "");
        assert_eq!(paths, vec!["/a", "/b"]);
    }

    #[test]
    fn extract_marker_with_missing_close_keeps_rest() {
        // Text after the malformed marker is preserved as-is
        let input = "<!--pidory:attach:/bad SUFFIX text";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(text, "<!--pidory:attach:/bad SUFFIX text");
        assert!(paths.is_empty());
    }

    // ── fence / inline backtick skip tests ──────────────────────────────────

    #[test]
    fn extract_in_fence_skipped() {
        // Marker inside a code fence must NOT be extracted; text is preserved.
        let input = "before\n```\n<!--pidory:attach:/secret.txt-->\n```\nafter";
        let (text, paths) = extract_file_markers(input);
        assert!(paths.is_empty(), "expected no paths, got: {:?}", paths);
        assert!(
            text.contains("<!--pidory:attach:/secret.txt-->"),
            "marker should be preserved inside fence, got: {:?}",
            text
        );
    }

    #[test]
    fn extract_outside_fence_extracted() {
        // Marker outside the fence is extracted normally; marker inside is not.
        let input = "<!--pidory:attach:/outside.txt-->```\n<!--pidory:attach:/inside.txt-->\n```";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(paths, vec!["/outside.txt"]);
        assert!(
            text.contains("<!--pidory:attach:/inside.txt-->"),
            "inside marker should be preserved, got: {:?}",
            text
        );
    }

    #[test]
    fn extract_in_inline_backtick_skipped() {
        // Marker wrapped in inline backtick must NOT be extracted.
        let input = "see `<!--pidory:attach:/inline.txt-->` for details";
        let (text, paths) = extract_file_markers(input);
        assert!(paths.is_empty(), "expected no paths, got: {:?}", paths);
        assert!(
            text.contains("<!--pidory:attach:/inline.txt-->"),
            "marker should be preserved inside inline, got: {:?}",
            text
        );
    }

    #[test]
    fn extract_fence_open_never_closed() {
        // Unclosed fence: all markers from the opening ``` onward are skipped.
        let input = "before ``` <!--pidory:attach:/trapped.txt--> no closing fence";
        let (text, paths) = extract_file_markers(input);
        assert!(paths.is_empty(), "expected no paths, got: {:?}", paths);
        assert!(
            text.contains("<!--pidory:attach:/trapped.txt-->"),
            "marker should be preserved, got: {:?}",
            text
        );
    }

    // ── multi-byte UTF-8 preservation tests ─────────────────────────────────

    #[test]
    fn extract_korean_in_fence_preserved() {
        // Korean text inside a fence must not be mojibaked — all chars preserved.
        let input = "```\n안녕 <!--pidory:attach:/x--> 한글\n```";
        let (text, paths) = extract_file_markers(input);
        assert!(paths.is_empty(), "expected no paths, got: {:?}", paths);
        assert!(text.contains("안녕"), "Korean chars lost, got: {:?}", text);
        assert!(text.contains("한글"), "Korean chars lost, got: {:?}", text);
        assert!(
            text.contains("<!--pidory:attach:/x-->"),
            "marker should be preserved inside fence, got: {:?}",
            text
        );
    }

    #[test]
    fn extract_multibyte_outside_fence_preserved() {
        // emoji + Japanese outside a fence: text is passed through intact,
        // and a marker after them is still extracted.
        let input = "🎉 日本語テスト <!--pidory:attach:/emoji.txt--> 終わり";
        let (text, paths) = extract_file_markers(input);
        assert_eq!(paths, vec!["/emoji.txt"]);
        assert!(text.contains("🎉"), "emoji lost, got: {:?}", text);
        assert!(text.contains("日本語テスト"), "Japanese lost, got: {:?}", text);
        assert!(text.contains("終わり"), "Japanese lost, got: {:?}", text);
    }

    // ── prepare_attachment async tests ───────────────────────────────────────

    #[tokio::test]
    async fn prepare_attachment_not_found() {
        let result = prepare_attachment("/nonexistent/__pidory_test__/file.txt").await;
        assert!(
            matches!(result, Err(FileAttachError::NotFound(_))),
            "expected NotFound, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn prepare_attachment_too_large() {
        use std::io::Write as _;

        let path = std::env::temp_dir().join("pidory_test_large.bin");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            // 26 MiB + 1 byte — just over the 25 MiB limit
            let chunk = vec![0u8; 1024];
            for _ in 0..26_215 {
                f.write_all(&chunk).unwrap();
            }
            // total: 26_215 * 1024 = 26_844_160 > 26_214_400
        }
        let result = prepare_attachment(path.to_str().unwrap()).await;
        let _ = std::fs::remove_file(&path);
        assert!(
            matches!(result, Err(FileAttachError::TooLarge { .. })),
            "expected TooLarge, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn prepare_attachment_success() {
        let path = std::env::temp_dir().join("pidory_test_hello.txt");
        std::fs::write(&path, b"hello world").unwrap();
        let result = prepare_attachment(path.to_str().unwrap()).await;
        let _ = std::fs::remove_file(&path);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[tokio::test]
    async fn prepare_attachment_empty_file() {
        let path = std::env::temp_dir().join("pidory_test_empty.txt");
        std::fs::write(&path, b"").unwrap();
        let result = prepare_attachment(path.to_str().unwrap()).await;
        let _ = std::fs::remove_file(&path);
        // Empty files are valid — no size restriction below 0
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }
}
