//! Integrity checking for settings files.
//!
//! sha256이 유일한 변경 검증 기준이다. mtime은 fingerprint 구조체에 보관하지만
//! `changed()` 비교에는 사용하지 않는다 (review #295 c2 fix).
//!
//! mtime을 trigger로 쓰던 이전 설계는 다음 케이스에서 false negative를 만들었다:
//! - mtime 1초 resolution OS (HFS+, FAT32) — 1초 미만 안에 두 write가 일어나면 mtime 동일
//! - mtime 보존 도구 — `touch -r`, `cp -p`, 일부 git checkout 동작
//! - vim atomic rename — inode가 바뀌어도 새 파일 mtime이 우연히 같을 수 있음
//!
//! sha256 only 비교는 위 모든 경우를 정확히 잡고, 같은 내용 re-write는 hash가
//! 같아 자연스럽게 false 반환된다.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use super::error::ClaudeSettingsError;

/// Snapshot of a file's last-modification time and content hash.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct FileFingerprint {
    pub(crate) mtime: SystemTime,
    pub(crate) sha256: [u8; 32],
}

/// Compute a [`FileFingerprint`] for an open file.
///
/// Steps:
/// 1. Read file metadata → `mtime`
/// 2. Seek to start → read full contents → SHA-256 hash
/// 3. Seek to start again so the caller can re-read the file
///
/// The file descriptor is left at position 0 after this call.
#[allow(dead_code)]
pub(crate) fn fingerprint(file: &mut File) -> Result<FileFingerprint, ClaudeSettingsError> {
    let mtime = file.metadata()?.modified()?;

    file.seek(SeekFrom::Start(0))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let hash_output = Sha256::digest(&buf);
    let sha256: [u8; 32] = hash_output.into();

    file.seek(SeekFrom::Start(0))?;

    Ok(FileFingerprint { mtime, sha256 })
}

/// Returns `true` if the file content has actually changed.
///
/// sha256 비교가 유일한 기준이다. mtime은 무시 (module doc 참조).
///
/// - `sha256` 같음 → `false` (내용 동일 — `touch`로 mtime만 바뀐 경우 포함)
/// - `sha256` 다름 → `true` (실제 변경)
#[allow(dead_code)]
pub(crate) fn changed(old: &FileFingerprint, new: &FileFingerprint) -> bool {
    old.sha256 != new.sha256
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use filetime::FileTime;
    use tempfile::NamedTempFile;

    use super::*;

    /// Helper: open a NamedTempFile as a std::fs::File for fingerprinting.
    fn reopen(tmp: &NamedTempFile) -> File {
        File::options()
            .read(true)
            .write(true)
            .open(tmp.path())
            .unwrap()
    }

    /// Test 1: same content re-written → changed == false
    ///
    /// mtime changes (we re-write) but sha256 stays the same, so `changed` must be false.
    #[test]
    fn same_content_rewrite_not_changed() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{{}}").unwrap();
        tmp.flush().unwrap();

        let mut f = reopen(&tmp);
        let fp_a = fingerprint(&mut f).unwrap();

        // Small sleep to ensure mtime can tick on coarse-resolution filesystems.
        std::thread::sleep(Duration::from_millis(10));

        // Re-write same content.
        {
            let mut f2 = File::options()
                .write(true)
                .truncate(true)
                .open(tmp.path())
                .unwrap();
            write!(f2, "{{}}").unwrap();
            f2.flush().unwrap();
        }

        let mut f = reopen(&tmp);
        let fp_b = fingerprint(&mut f).unwrap();

        assert!(
            !changed(&fp_a, &fp_b),
            "same content re-write should not be considered changed"
        );
    }

    /// Test 2: different content → changed == true
    #[test]
    fn different_content_is_changed() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{{}}").unwrap();
        tmp.flush().unwrap();

        let mut f = reopen(&tmp);
        let fp_a = fingerprint(&mut f).unwrap();

        std::thread::sleep(Duration::from_millis(10));

        {
            let mut f2 = File::options()
                .write(true)
                .truncate(true)
                .open(tmp.path())
                .unwrap();
            write!(f2, "{{\"x\":1}}").unwrap();
            f2.flush().unwrap();
        }

        let mut f = reopen(&tmp);
        let fp_b = fingerprint(&mut f).unwrap();

        assert!(
            changed(&fp_a, &fp_b),
            "different content should be considered changed"
        );
    }

    /// Test 3: mtime forced identical, different content → changed == true
    ///
    /// review #295 c2 fix: `changed`는 sha256만 비교한다. mtime이 같아도 sha256이
    /// 다르면 변경으로 판정 — coarse-resolution FS, mtime 보존 편집 도구 등의
    /// false negative를 차단한다.
    #[test]
    fn forced_same_mtime_different_content_is_changed() {
        let mut tmp_a = NamedTempFile::new().unwrap();
        write!(tmp_a, "{{}}").unwrap();
        tmp_a.flush().unwrap();

        let mut tmp_b = NamedTempFile::new().unwrap();
        write!(tmp_b, "{{\"x\":1}}").unwrap();
        tmp_b.flush().unwrap();

        // Force both files to share the same mtime.
        let fixed_time = FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(tmp_a.path(), fixed_time).unwrap();
        filetime::set_file_mtime(tmp_b.path(), fixed_time).unwrap();

        let mut fa = reopen(&tmp_a);
        let mut fb = reopen(&tmp_b);
        let fp_a = fingerprint(&mut fa).unwrap();
        let fp_b = fingerprint(&mut fb).unwrap();

        assert!(
            changed(&fp_a, &fp_b),
            "sha256 differs → changed must be true regardless of mtime"
        );
    }

    /// Test 4: sha256 accuracy — empty input has known hash
    ///
    /// SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    #[test]
    fn sha256_empty_known_hash() {
        let tmp = NamedTempFile::new().unwrap();
        // File is empty; nothing written.

        let mut f = reopen(&tmp);
        let fp = fingerprint(&mut f).unwrap();

        let expected: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(
            fp.sha256, expected,
            "SHA-256 of empty input must match known value"
        );
    }
}
