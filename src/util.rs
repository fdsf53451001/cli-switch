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

pub fn ensure_parent(path: &Path) -> R<()> {
    if let Some(p) = path.parent() {
        ensure_dir(p)?;
    }
    Ok(())
}

/// Write atomically: write to `<path>.tmp` then rename over the target.
pub fn write_atomic(path: &Path, contents: &str) -> R<()> {
    ensure_parent(path)?;
    let tmp = path.with_extension(tmp_ext(path));
    fs::write(&tmp, contents).map_err(|e| ctx(&tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| ctx(path, e))
}

fn tmp_ext(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.cli-switch-tmp"),
        None => "cli-switch-tmp".to_string(),
    }
}

/// Copy a file to the backups dir with a timestamp-free, path-derived name.
/// We keep one backup per source so repeated runs don't accumulate forever;
/// the `.bak` is overwritten each sync, which is the last-good snapshot.
pub fn backup_file(src: &Path, backups_dir: &Path) -> R<Option<PathBuf>> {
    if !src.exists() {
        return Ok(None);
    }
    ensure_dir(backups_dir)?;
    let flat = src
        .to_string_lossy()
        .replace(['/', '\\', ':'], "_")
        .trim_start_matches('_')
        .to_string();
    let dest = backups_dir.join(format!("{flat}.bak"));
    // Atomic: copy to a temp then rename over the .bak, so an interrupted copy
    // can't truncate the last-good backup (matches write_atomic's discipline).
    let tmp = backups_dir.join(format!("{flat}.bak.tmp"));
    fs::copy(src, &tmp).map_err(|e| ctx(&tmp, e))?;
    fs::rename(&tmp, &dest).map_err(|e| ctx(&dest, e))?;
    Ok(Some(dest))
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
                fs::write(path, b"").map_err(|e| ctx(path, e))?;
                Ok(Some(Lock {
                    path: path.to_path_buf(),
                }))
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
