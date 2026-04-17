use std::sync::Arc;

use poise::serenity_prelude as serenity;

use crate::update;
use crate::{Context, Error};

// ── GitHub API helper ─────────────────────────────────────────────────────────

async fn fetch_latest_tag(repo: &str, token: Option<&str>) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
    let client = reqwest::Client::builder()
        .user_agent("pidory")
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API status {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    json.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "tag_name missing".into())
}

// ── Status edit helper ────────────────────────────────────────────────────────

async fn update_status(
    reply: &poise::ReplyHandle<'_>,
    ctx: Context<'_>,
    content: String,
) -> Result<(), Error> {
    let msg = poise::CreateReply::default().content(content);
    reply.edit(ctx, msg).await?;
    Ok(())
}

// ── Command ───────────────────────────────────────────────────────────────────

/// 봇을 최신 릴리스 태그로 자가 업데이트한다 (owner 전용)
#[poise::command(slash_command, guild_only)]
pub async fn update(
    ctx: Context<'_>,
    #[description = "강제 재빌드/재시작 (활성 턴 무시, 동일 버전도 재빌드)"]
    force: Option<bool>,
) -> Result<(), Error> {
    let data = ctx.data();
    let lang = data.config.language;
    let force = force.unwrap_or(false);

    // ── Step 1: owner 권한 체크 ───────────────────────────────────────────────
    let is_owner = ctx.author().id == serenity::UserId::new(data.config.discord.owner_id);
    if !is_owner {
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.no_permission()))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    // ── Step 2: 초기 응답 메시지 ──────────────────────────────────────────────
    let reply = ctx
        .send(poise::CreateReply::default().content("🔍 업데이트 준비 중..."))
        .await?;

    // ── Step 3: worktree 감지 + sanity ────────────────────────────────────────
    let worktree = match update::worktree::detect_worktree() {
        Ok(p) => p,
        Err(e) => {
            update_status(&reply, ctx, format!("❌ worktree 감지 실패: {}", e)).await?;
            return Ok(());
        }
    };
    if let Err(e) = update::worktree::sanity_check(&worktree) {
        update_status(&reply, ctx, format!("❌ worktree sanity 실패: {}", e)).await?;
        return Ok(());
    }

    // ── Step 4: 락 획득 ───────────────────────────────────────────────────────
    let _lock = match update::lock::acquire(&worktree) {
        Ok(guard) => guard,
        Err(update::Error::LockHeld(pid)) => {
            update_status(
                &reply,
                ctx,
                format!("❌ 이미 업데이트 진행 중 (PID={})", pid),
            )
            .await?;
            return Ok(());
        }
        Err(e) => {
            update_status(&reply, ctx, format!("❌ 락 획득 실패: {}", e)).await?;
            return Ok(());
        }
    };

    // ── Step 5: dirty 체크 ────────────────────────────────────────────────────
    let is_dirty = match update::worktree::is_dirty(&worktree) {
        Ok(d) => d,
        Err(e) => {
            update_status(&reply, ctx, format!("❌ dirty 체크 실패: {}", e)).await?;
            return Ok(());
        }
    };
    if is_dirty {
        update_status(&reply, ctx, lang.update_dirty_tree().to_string()).await?;
        return Ok(());
    }

    // ── Step 6: 활성 턴 체크 ─────────────────────────────────────────────────
    let active_threads: Vec<String> = data
        .sessions
        .get_session_info()
        .await
        .into_iter()
        .filter(|info| info.is_turn_active)
        .map(|info| info.thread_id.clone())
        .collect();
    if !active_threads.is_empty() && !force {
        update_status(&reply, ctx, lang.update_active_turns(&active_threads)).await?;
        return Ok(());
    }

    // ── Step 7: 최신 태그 조회 ────────────────────────────────────────────────
    update_status(&reply, ctx, "🔍 최신 릴리스 확인 중...".to_string()).await?;
    let token = data
        .config
        .release
        .token_env
        .as_deref()
        .and_then(|env_name| std::env::var(env_name).ok());
    let latest_tag = match fetch_latest_tag(&data.config.release.repo, token.as_deref()).await {
        Ok(tag) => tag,
        Err(e) => {
            update_status(&reply, ctx, format!("❌ 최신 태그 조회 실패: {}", e)).await?;
            return Ok(());
        }
    };

    // ── Step 8: 버전 비교 ─────────────────────────────────────────────────────
    let current = update::version::current_version();
    if !update::version::needs_update(current, &latest_tag, force) {
        update_status(&reply, ctx, lang.update_already_latest(current)).await?;
        return Ok(());
    }

    // ── Step 9: 디스크 공간 검사 ──────────────────────────────────────────────
    update_status(&reply, ctx, "💾 디스크 공간 확인 중...".to_string()).await?;
    if let Err(e) = update::backup::check_disk_space(&worktree, 2 * 1024 * 1024 * 1024) {
        update_status(&reply, ctx, format!("❌ 디스크 공간 부족: {}", e)).await?;
        return Ok(());
    }

    // ── Step 10: git fetch ────────────────────────────────────────────────────
    update_status(
        &reply,
        ctx,
        format!("📡 태그 fetch 중... ({}→{})", current, latest_tag),
    )
    .await?;
    if let Err(e) = update::git::fetch_tags(&worktree).await {
        update_status(&reply, ctx, format!("❌ git fetch 실패: {}", e)).await?;
        return Ok(());
    }

    // ── Step 11: git checkout ─────────────────────────────────────────────────
    update_status(
        &reply,
        ctx,
        format!("🔀 {} 체크아웃 중...", latest_tag),
    )
    .await?;
    if let Err(e) = update::git::checkout_tag(&worktree, &latest_tag).await {
        update_status(&reply, ctx, format!("❌ git checkout 실패: {}", e)).await?;
        return Ok(());
    }

    // ── Step 12: 바이너리 백업 ────────────────────────────────────────────────
    update_status(&reply, ctx, "📦 바이너리 백업 중...".to_string()).await?;
    if let Err(e) = update::backup::backup_binary(&worktree) {
        update_status(&reply, ctx, format!("❌ 바이너리 백업 실패: {}", e)).await?;
        return Ok(());
    }

    // ── Step 13: DB 백업 ──────────────────────────────────────────────────────
    if let Err(e) = update::backup::backup_db(std::path::Path::new(&data.config.database.path)) {
        update_status(&reply, ctx, format!("❌ DB 백업 실패: {}", e)).await?;
        return Ok(());
    }

    // ── Step 14: 빌드 ─────────────────────────────────────────────────────────
    update_status(&reply, ctx, "🔨 빌드 중... (수 분 소요될 수 있습니다)".to_string()).await?;
    let build_start = std::time::Instant::now();
    let line_counter = Arc::new(std::sync::Mutex::new(0usize));
    let counter_clone = Arc::clone(&line_counter);
    let _duration = match update::build::build_release(&worktree, move |_line| {
        let mut c = counter_clone.lock().unwrap_or_else(|e| e.into_inner());
        *c += 1;
    })
    .await
    {
        Ok(d) => d,
        Err(update::Error::BuildFailed { stderr_tail }) => {
            update_status(&reply, ctx, lang.update_build_failed(&stderr_tail)).await?;
            return Ok(());
        }
        Err(e) => {
            update_status(&reply, ctx, format!("❌ 빌드 실패: {}", e)).await?;
            return Ok(());
        }
    };
    let line_count = *line_counter.lock().unwrap_or_else(|e| e.into_inner());
    let build_secs = build_start.elapsed().as_secs();
    update_status(
        &reply,
        ctx,
        format!("✅ 빌드 완료 ({}s, {} 라인)", build_secs, line_count),
    )
    .await?;

    // ── Step 14.5: skills sync ────────────────────────────────────────────────
    update_status(&reply, ctx, "📚 skills 동기화 중...".to_string()).await?;
    match update::skills::sync_skills(&worktree) {
        Ok(n) => {
            tracing::info!("synced {} skills", n);
        }
        Err(e) => {
            update_status(&reply, ctx, format!("❌ skills sync 실패: {}", e)).await?;
            return Ok(());
        }
    }

    // ── Step 15: 마커 생성 ────────────────────────────────────────────────────
    if let Err(e) = update::marker::create_marker(&worktree, current, &latest_tag) {
        update_status(&reply, ctx, format!("❌ 마커 생성 실패: {}", e)).await?;
        return Ok(());
    }

    // ── Step 16: 재시작 스케줄 ────────────────────────────────────────────────
    let new_version = latest_tag.strip_prefix('v').unwrap_or(&latest_tag);
    if let Err(e) = update::restart::schedule_restart() {
        update_status(
            &reply,
            ctx,
            format!(
                "❌ 재시작 스케줄 실패: {}\n(빌드는 성공했습니다. 수동으로 재시작하세요.)",
                e
            ),
        )
        .await?;
        return Ok(());
    }

    update_status(&reply, ctx, lang.update_complete(new_version)).await?;
    Ok(())
}
