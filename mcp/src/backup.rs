//! Backup files on the server: the backup directory, a unique file per download, the digest the
//! download announces, and removal of whatever a download leaves behind.
//!
//! A backup holds a copy of the universe, private archive included: files are created mode 0600
//! (see `af_store::Store::create_export`) in a directory made mode 0700, and a download's file is
//! removed as soon as it has been sent, or when the request fails or is abandoned.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// Prefix of the files `GET /api/sysop/backup` writes (one per request, removed after it).
pub const DOWNLOAD_PREFIX: &str = "download-";

/// Leftovers of an interrupted backup this old are removed at startup (a running backup touches
/// its file continuously, so a stale one is not in use).
const STALE_AFTER: Duration = Duration::from_secs(3600);

/// Create the backup directory (mode 0700 on Unix when this creates it) and remove what earlier
/// runs left behind: the fixed-name download files of older builds, and stale download files and
/// partial copies. Returns how many files it removed.
pub fn prepare_dir(dir: &Path) -> std::io::Result<usize> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)?;
    let mut removed = 0;
    let now = SystemTime::now();
    for e in std::fs::read_dir(dir)?.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let legacy = name == "download.redb" || name == "download.partial";
        let ours = name.starts_with(DOWNLOAD_PREFIX) || name.ends_with(".partial");
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if (legacy || (ours && stale)) && e.file_type().is_ok_and(|t| t.is_file()) && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// A path for one download's backup, used by no other request.
pub fn download_path(dir: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        "{DOWNLOAD_PREFIX}{}-{}-{n}.redb",
        crate::game::now_ms(),
        std::process::id()
    ))
}

/// Size and lowercase hex SHA-256 of a file, read in 1 MiB blocks.
pub fn digest(path: &Path) -> std::io::Result<(u64, String)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut len = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
    }
    Ok((len, hex::encode(h.finalize())))
}

/// Removes a file when dropped: a download's backup goes when the response has been sent or the
/// request fails. (A copy still being written belongs to its job, which removes it when nobody
/// waits for it any more: see `Game::backup_with`.)
pub struct RemoveOnDrop(pub PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_dir_removes_leftovers_but_not_backups() {
        let dir = std::env::temp_dir().join(format!("af-backup-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        prepare_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        for f in ["download.redb", "download.partial", "auto-1.redb", "manual-1.redb"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let fresh = download_path(&dir);
        std::fs::write(&fresh, b"x").unwrap();
        assert_eq!(prepare_dir(&dir).unwrap(), 2, "the legacy fixed names go");
        assert!(fresh.exists(), "a fresh download file may still be in use");
        assert!(dir.join("auto-1.redb").exists() && dir.join("manual-1.redb").exists());
        assert_ne!(download_path(&dir), download_path(&dir));
        let (len, sha) = digest(&fresh).unwrap();
        assert_eq!(len, 1);
        assert_eq!(sha, "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881");
        {
            let _guard = RemoveOnDrop(fresh.clone());
        }
        assert!(!fresh.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
