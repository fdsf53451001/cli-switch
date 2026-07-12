//! Small filesystem helpers shared across modules.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub type R<T> = Result<T, String>;

/// Turn any error into a String error carrying the path for context.
pub fn ctx<E: std::fmt::Display>(path: &Path, e: E) -> String {
    format!("{}: {}", path.display(), e)
}

pub fn read_to_string_opt(path: &Path) -> R<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ctx(path, e)),
    }
}

pub fn ensure_dir(path: &Path) -> R<()> {
    fs::create_dir_all(path).map_err(|e| ctx(path, e))
}

pub fn ensure_private_dir(path: &Path) -> R<()> {
    ensure_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| ctx(path, e))?;
    }
    Ok(())
}

pub fn write_private(path: &Path, contents: &[u8]) -> R<()> {
    write_atomic_bytes(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| ctx(path, e))?;
    }
    Ok(())
}

pub fn ensure_parent(path: &Path) -> R<()> {
    if let Some(p) = path.parent() {
        ensure_dir(p)?;
    }
    Ok(())
}

/// Write atomically: write to `<path>.tmp` then rename over the target.
pub fn write_atomic(path: &Path, contents: &str) -> R<()> {
    write_atomic_bytes(path, contents.as_bytes())
}

pub fn write_atomic_bytes(path: &Path, contents: &[u8]) -> R<()> {
    ensure_parent(path)?;
    let tmp = path.with_extension(format!("{}.{}", tmp_ext(path), std::process::id()));
    let _ = fs::remove_file(&tmp);
    fs::write(&tmp, contents).map_err(|e| ctx(&tmp, e))?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(first) if path.exists() => {
            fs::remove_file(path).map_err(|e| ctx(path, e))?;
            fs::rename(&tmp, path)
                .map_err(|e| format!("{} (initial replace error: {first})", ctx(path, e)))
        }
        Err(e) => Err(ctx(path, e)),
    }
}

pub fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Stable, non-cryptographic content fingerprint used for stale-plan checks.
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn tmp_ext(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.cli-switch-tmp"),
        None => "cli-switch-tmp".to_string(),
    }
}

/// A best-effort exclusive lock so concurrent startup-hook syncs don't collide.
/// Held for the process lifetime; removed on drop.
pub struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Try to acquire the lock. Returns `None` if another fresh run holds it.
/// A lock older than `stale_secs` is considered abandoned and stolen.
pub fn acquire_lock(path: &Path, stale_secs: u64) -> R<Option<Lock>> {
    ensure_parent(path)?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(Some(Lock {
            path: path.to_path_buf(),
        })),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let age = now_secs().saturating_sub(mtime_secs(path));
            if age > stale_secs {
                let _ = fs::remove_file(path);
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    Ok(_) => Ok(Some(Lock {
                        path: path.to_path_buf(),
                    })),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
                    Err(e) => Err(ctx(path, e)),
                }
            } else {
                Ok(None)
            }
        }
        Err(e) => Err(ctx(path, e)),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// File modification time as seconds since epoch (0 if unavailable).
pub fn mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
