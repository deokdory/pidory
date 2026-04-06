use poise::serenity_prelude::{ChannelId, Context, CreateEmbed, CreateMessage};
use serde::Deserialize;

use crate::i18n::Lang;

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
}

pub struct ReleaseChecker {
    client: reqwest::Client,
    repo: String,
    last_tag_file: String,
}

impl ReleaseChecker {
    pub fn new(repo: String, last_tag_file: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("pidory")
            .build()
            .unwrap_or_default();
        Self {
            client,
            repo,
            last_tag_file,
        }
    }

    async fn fetch_latest(&self) -> Option<GitHubRelease> {
        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.repo
        );
        let response = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to fetch latest release from {url}: {e}");
                return None;
            }
        };

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::warn!("no releases found for repo {}", self.repo);
            return None;
        }

        if !response.status().is_success() {
            let status = response.status();
            tracing::warn!("GitHub releases API returned non-success status {status} for {url}");
            return None;
        }

        match response.json::<GitHubRelease>().await {
            Ok(release) => Some(release),
            Err(e) => {
                tracing::warn!("failed to parse GitHub release response: {e}");
                None
            }
        }
    }

    fn read_last_tag(&self) -> Option<String> {
        std::fs::read_to_string(&self.last_tag_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn write_last_tag(&self, tag: &str) {
        if let Err(e) = std::fs::write(&self.last_tag_file, tag) {
            tracing::warn!("failed to write last tag to {}: {e}", self.last_tag_file);
        }
    }

    pub fn should_notify(last_tag: Option<&str>, current_tag: &str) -> bool {
        match last_tag {
            None => false,
            Some(last) => last != current_tag,
        }
    }

    pub fn truncate_body(body: &str, max_len: usize, truncation_suffix: &str) -> String {
        if body.chars().count() <= max_len {
            return body.to_string();
        }

        let suffix_len = truncation_suffix.chars().count();
        let cut_at = max_len.saturating_sub(suffix_len);

        // Find char boundary at cut_at
        let byte_pos = body
            .char_indices()
            .nth(cut_at)
            .map(|(i, _)| i)
            .unwrap_or(body.len());

        let truncated = &body[..byte_pos];

        // Try to cut at last newline boundary
        let final_pos = truncated
            .rfind('\n')
            .unwrap_or(byte_pos);

        let trimmed = body[..final_pos].trim_end();
        format!("{}\n{}", trimmed, truncation_suffix)
    }

    pub async fn check_and_notify(&self, ctx: &Context, channel_id: ChannelId, lang: Lang) {
        let release = match self.fetch_latest().await {
            Some(r) => r,
            None => return,
        };

        let last_tag = self.read_last_tag();

        if !Self::should_notify(last_tag.as_deref(), &release.tag_name) {
            if last_tag.is_none() {
                // First boot — store the tag without notifying
                self.write_last_tag(&release.tag_name);
            }
            return;
        }

        let description = match &release.body {
            Some(body) if !body.trim().is_empty() => {
                let suffix = lang.release_body_truncated(&release.html_url);
                Self::truncate_body(body, 4096, &suffix)
            }
            _ => lang.release_no_body().to_string(),
        };

        let embed = CreateEmbed::new()
            .color(0x5865F2u32)
            .title(lang.release_notify_title(&release.tag_name))
            .url(&release.html_url)
            .description(&description);

        let message = CreateMessage::new().embed(embed);

        if let Err(e) = channel_id.send_message(ctx, message).await {
            tracing::warn!("failed to send release notification: {e}");
            return;
        }

        self.write_last_tag(&release.tag_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── should_notify ──

    #[test]
    fn should_notify_first_boot() {
        // None(최초 기동) → false
        assert!(!ReleaseChecker::should_notify(None, "v1.0.0"));
    }

    #[test]
    fn should_notify_same_tag() {
        // 같은 태그 → false
        assert!(!ReleaseChecker::should_notify(Some("v1.0.0"), "v1.0.0"));
    }

    #[test]
    fn should_notify_different_tag() {
        // 다른 태그 → true
        assert!(ReleaseChecker::should_notify(Some("v1.0.0"), "v1.1.0"));
    }

    // ── truncate_body ──

    #[test]
    fn truncate_body_short() {
        // max_len 이하 → 그대로 반환
        let body = "short body";
        let result = ReleaseChecker::truncate_body(body, 100, "...");
        assert_eq!(result, body);
    }

    #[test]
    fn truncate_body_long() {
        // 초과 시 truncation
        let body = "a".repeat(5000);
        let suffix = "… [more](url)";
        let result = ReleaseChecker::truncate_body(&body, 4096, suffix);
        assert!(result.chars().count() <= 4096 + suffix.chars().count() + 1); // +1 for newline
        assert!(result.ends_with(suffix));
    }

    #[test]
    fn truncate_body_at_newline_boundary() {
        // 줄바꿈 경계에서 잘림
        let body = format!("{}\n{}", "a".repeat(100), "b".repeat(100));
        let suffix = "...";
        let result = ReleaseChecker::truncate_body(&body, 110, suffix);
        assert!(result.contains(&"a".repeat(100)));
        assert!(!result.contains('b')); // b가 있는 줄은 잘렸어야 함
        assert!(result.ends_with(suffix));
    }

    #[test]
    fn truncate_body_exact_boundary() {
        // 정확히 max_len → truncation 안 됨
        let body = "a".repeat(4096);
        let result = ReleaseChecker::truncate_body(&body, 4096, "...");
        assert_eq!(result, body);
    }

    // ── read_last_tag / write_last_tag ──

    #[test]
    fn read_last_tag_missing_file() {
        let checker = ReleaseChecker::new(
            "test/repo".to_string(),
            "/nonexistent/path/last-release.txt".to_string(),
        );
        assert!(checker.read_last_tag().is_none());
    }

    #[test]
    fn write_and_read_last_tag() {
        let dir = std::env::temp_dir().join("pidory_test_release_tag");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("last-release.txt");

        let checker = ReleaseChecker::new(
            "test/repo".to_string(),
            path.to_str().unwrap().to_string(),
        );

        checker.write_last_tag("v1.0.0");
        let tag = checker.read_last_tag();
        assert_eq!(tag.as_deref(), Some("v1.0.0"));

        // cleanup
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_last_tag_trims_whitespace() {
        let dir = std::env::temp_dir().join("pidory_test_release_trim");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("last-release-trim.txt");
        std::fs::write(&path, "  v1.0.0  \n").unwrap();

        let checker = ReleaseChecker::new(
            "test/repo".to_string(),
            path.to_str().unwrap().to_string(),
        );

        assert_eq!(checker.read_last_tag().as_deref(), Some("v1.0.0"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_last_tag_empty_file() {
        let dir = std::env::temp_dir().join("pidory_test_release_empty");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("last-release-empty.txt");
        std::fs::write(&path, "").unwrap();

        let checker = ReleaseChecker::new(
            "test/repo".to_string(),
            path.to_str().unwrap().to_string(),
        );

        assert!(checker.read_last_tag().is_none());
        std::fs::remove_file(&path).ok();
    }
}
