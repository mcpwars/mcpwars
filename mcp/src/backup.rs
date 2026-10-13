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
    rotate_manual(dir)?;
    Ok(removed)
}

/// Defaults leave room for retained files and other runtime users of a 2 GiB volume.
pub const ONLINE_MAX_BYTES: u64 = 512 << 20;
const DIRECTORY_MAX_BYTES: u64 = 1 << 30;
const FREE_RESERVE_BYTES: u64 = 128 << 20;
const MEMORY_RESERVE_BYTES: u64 = 512 << 20;

/// Refuse before copying if the directory or filesystem has no safe budget. Run on a
/// blocking worker under the backup permit. The store enforces the returned cap at every write.
pub fn budget(dest: &Path) -> Result<u64, String> {
    let dir = dest.parent().ok_or("backup has no parent directory")?;
    memory_headroom(cgroup_bytes("memory.current"), cgroup_bytes("memory.max"))?;
    let used = directory_bytes(dir).map_err(|e| format!("backup disk check: {e}"))?;
    let configured = match std::env::var("AF_BACKUP_MAX_BYTES") {
        Ok(v) => v
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or("AF_BACKUP_MAX_BYTES must be a positive byte count")?,
        Err(std::env::VarError::NotPresent) => ONLINE_MAX_BYTES,
        Err(e) => return Err(format!("AF_BACKUP_MAX_BYTES: {e}")),
    };
    let mut cap = configured.min(DIRECTORY_MAX_BYTES.saturating_sub(used));
    #[cfg(unix)]
    {
        let free = free_bytes(dir)?;
        cap = cap.min(free.saturating_sub(FREE_RESERVE_BYTES));
    }
    if cap < 16 << 20 {
        return Err("not enough safe disk space for an online backup; export on a separate machine".into());
    }
    tracing::info!(directory_bytes = used, max_bytes = cap, "online backup disk budget");
    log_resources(dir);
    Ok(cap)
}

fn directory_bytes(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

#[cfg(unix)]
fn free_bytes(dir: &Path) -> Result<u64, String> {
    // POSIX df avoids platform-specific unsafe statvfs bindings. No shell or credentials.
    let out = std::process::Command::new("df")
        .args(["-Pk", "--"])
        .arg(dir)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("backup free-space check: {e}"))?;
    if !out.status.success() {
        return Err("backup free-space check failed".into());
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(|v| v.checked_mul(1024))
        .ok_or_else(|| "backup free-space check returned no usable result".into())
}

/// Retain the newest completed manual backup. A failed new backup never removes the last good one.
pub fn rotate_manual(dir: &Path) -> std::io::Result<()> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("manual-") && name.ends_with(".redb") && entry.file_type()?.is_file() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    paths.pop();
    for path in paths {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn cgroup_bytes(name: &str) -> Option<u64> {
    std::fs::read_to_string(format!("/sys/fs/cgroup/{name}"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn memory_headroom(current: Option<u64>, limit: Option<u64>) -> Result<(), String> {
    if let (Some(current), Some(limit)) = (current, limit)
        && limit.saturating_sub(current) < MEMORY_RESERVE_BYTES
    {
        return Err("not enough memory headroom for an online backup (512 MiB required); export on a separate machine".into());
    }
    Ok(())
}

/// Operational counters only: no game data or credentials. Linux supplies cgroup RAM.
pub fn log_resources(dir: &Path) {
    tracing::info!(
        memory_current_bytes = ?cgroup_bytes("memory.current"),
        memory_limit_bytes = ?cgroup_bytes("memory.max"),
        memory_peak_bytes = ?cgroup_bytes("memory.peak"),
        backup_directory_bytes = ?directory_bytes(dir).ok(),
        "runtime resources"
    );
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
    fn backup_requires_memory_headroom_when_a_limit_is_known() {
        assert!(memory_headroom(Some(1600 << 20), Some(2048 << 20)).is_err());
        assert!(memory_headroom(Some(1536 << 20), Some(2048 << 20)).is_ok());
        assert!(memory_headroom(Some(2200 << 20), Some(2048 << 20)).is_err());
        assert!(memory_headroom(None, Some(2048 << 20)).is_ok());
        assert!(memory_headroom(Some(1600 << 20), None).is_ok());
    }

    #[test]
    fn manual_retention_preserves_newest_and_other_files() {
        let dir = std::env::temp_dir().join(format!("af-backup-retention-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        prepare_dir(&dir).unwrap();
        for name in [
            "manual-100.redb",
            "manual-200.redb",
            "auto-100.redb",
            "download-100.redb",
            "manual-300.partial",
        ] {
            std::fs::write(dir.join(name), b"backup").unwrap();
        }
        rotate_manual(&dir).unwrap();
        assert!(!dir.join("manual-100.redb").exists());
        for name in ["manual-200.redb", "auto-100.redb", "download-100.redb", "manual-300.partial"] {
            assert!(dir.join(name).exists());
        }
        assert_eq!(directory_bytes(&dir).unwrap(), 24);
        #[cfg(unix)]
        assert!(free_bytes(&dir).unwrap() > 0);
        // Sparse files exercise the aggregate budget without consuming a GiB of disk.
        std::fs::File::create(dir.join("unrelated"))
            .unwrap()
            .set_len(DIRECTORY_MAX_BYTES)
            .unwrap();
        assert!(
            budget(&dir.join("download-next.redb"))
                .unwrap_err()
                .contains("not enough safe disk")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

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
