use std::fs;
use std::io;
use std::path::Path;

use super::Error;

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_all(&s, &d)?;
        } else if ft.is_file() {
            fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

pub fn sync_skills(worktree: &Path) -> Result<usize, Error> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| Error::SkillSyncFailed("HOME not set".into()))?;
    sync_skills_to(worktree, &Path::new(&home).join(".claude").join("skills"))
}

pub(super) fn sync_skills_to(worktree: &Path, target_root: &Path) -> Result<usize, Error> {
    let skills_src = worktree.join("skills");

    if !skills_src.exists() {
        return Ok(0);
    }

    fs::create_dir_all(target_root)
        .map_err(|e| Error::SkillSyncFailed(format!("failed to create target dir: {e}")))?;

    let entries = fs::read_dir(&skills_src)
        .map_err(|e| Error::SkillSyncFailed(format!("failed to read skills dir: {e}")))?;

    let mut count = 0usize;

    for entry in entries {
        let entry = entry
            .map_err(|e| Error::SkillSyncFailed(format!("failed to read dir entry: {e}")))?;

        let ft = entry
            .file_type()
            .map_err(|e| Error::SkillSyncFailed(format!("failed to get file type: {e}")))?;

        if !ft.is_dir() {
            continue;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip names with path separators
        if name_str.contains('/') || name_str.contains('\\') {
            continue;
        }

        let src_skill = entry.path();
        let staging = target_root.join(format!("{}.staging", name_str));
        let final_dir = target_root.join(&*name_str);
        let old_dir = target_root.join(format!("{}.old", name_str));

        // 1. Clean up leftover staging from previous failed run
        if staging.exists() {
            fs::remove_dir_all(&staging).map_err(|e| {
                Error::SkillSyncFailed(format!(
                    "failed to clean staging {}: {e}",
                    staging.display()
                ))
            })?;
        }

        // 2. Copy src -> staging
        if let Err(e) = copy_dir_all(&src_skill, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(Error::SkillSyncFailed(format!(
                "failed to copy skill '{}': {e}",
                name_str
            )));
        }

        // 3. If final exists, rename final -> old
        if final_dir.exists()
            && let Err(e) = fs::rename(&final_dir, &old_dir)
        {
            let _ = fs::remove_dir_all(&staging);
            return Err(Error::SkillSyncFailed(format!(
                "failed to move '{}' to old: {e}",
                name_str
            )));
        }

        // 4. staging -> final
        if let Err(e) = fs::rename(&staging, &final_dir) {
            // Attempt recovery: old -> final
            if old_dir.exists() {
                let _ = fs::rename(&old_dir, &final_dir);
            }
            let _ = fs::remove_dir_all(&staging);
            return Err(Error::SkillSyncFailed(format!(
                "failed to rename staging to '{}': {e}",
                name_str
            )));
        }

        // 5. Remove old
        if old_dir.exists() {
            fs::remove_dir_all(&old_dir).map_err(|e| {
                Error::SkillSyncFailed(format!("failed to remove old '{}': {e}", name_str))
            })?;
        }

        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_skill(dir: &Path, name: &str, files: &[(&str, &str)]) {
        let skill_dir = dir.join("skills").join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        for (fname, content) in files {
            fs::write(skill_dir.join(fname), content).unwrap();
        }
    }

    #[test]
    fn test_normal_sync() {
        let worktree = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        make_skill(worktree.path(), "foo", &[("SKILL.md", "hello skill")]);

        let count = sync_skills_to(worktree.path(), target.path()).unwrap();
        assert_eq!(count, 1);

        let content =
            fs::read_to_string(target.path().join("foo").join("SKILL.md")).unwrap();
        assert_eq!(content, "hello skill");
    }

    #[test]
    fn test_overwrite_existing_skill() {
        let worktree = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        // Pre-populate target with old file
        let old_skill = target.path().join("foo");
        fs::create_dir_all(&old_skill).unwrap();
        fs::write(old_skill.join("OLD.md"), "old content").unwrap();

        make_skill(worktree.path(), "foo", &[("SKILL.md", "new content")]);

        let count = sync_skills_to(worktree.path(), target.path()).unwrap();
        assert_eq!(count, 1);

        // New file exists
        let content =
            fs::read_to_string(target.path().join("foo").join("SKILL.md")).unwrap();
        assert_eq!(content, "new content");

        // Old file gone — atomic rename replaced entire dir
        assert!(!target.path().join("foo").join("OLD.md").exists());
    }

    #[test]
    fn test_other_source_skill_preserved() {
        let worktree = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        // bar/ exists in target but NOT in worktree
        let bar_dir = target.path().join("bar");
        fs::create_dir_all(&bar_dir).unwrap();
        fs::write(bar_dir.join("BAR.md"), "bar skill").unwrap();

        make_skill(worktree.path(), "foo", &[("SKILL.md", "foo skill")]);

        let count = sync_skills_to(worktree.path(), target.path()).unwrap();
        assert_eq!(count, 1);

        // bar/ still intact
        assert!(target.path().join("bar").join("BAR.md").exists());
        let bar_content =
            fs::read_to_string(target.path().join("bar").join("BAR.md")).unwrap();
        assert_eq!(bar_content, "bar skill");
    }

    #[test]
    fn test_no_skills_dir_returns_zero() {
        let worktree = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        // No skills/ directory in worktree at all
        let count = sync_skills_to(worktree.path(), target.path()).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_staging_leftover_cleaned_up() {
        let worktree = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        // Simulate a leftover staging dir from a previous failed run
        let leftover_staging = target.path().join("foo.staging");
        fs::create_dir_all(&leftover_staging).unwrap();
        fs::write(leftover_staging.join("stale.md"), "stale").unwrap();

        make_skill(worktree.path(), "foo", &[("SKILL.md", "fresh content")]);

        let count = sync_skills_to(worktree.path(), target.path()).unwrap();
        assert_eq!(count, 1);

        // Staging dir cleaned up
        assert!(!target.path().join("foo.staging").exists());

        // Final dir has fresh content
        let content =
            fs::read_to_string(target.path().join("foo").join("SKILL.md")).unwrap();
        assert_eq!(content, "fresh content");
    }
}
