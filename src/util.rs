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
    if let Err(e) = fs::write(&tmp, contents) {
        let _ = fs::remove_file(&tmp);
        return Err(ctx(&tmp, e));
    }
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

/// Serializes `Vec<u8>` fields as base64 instead of serde_json's default
/// one-JSON-number-per-byte array, which bloats journal/state snapshots
/// several-fold for anything but tiny files.
pub mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied();
            let b2 = chunk.get(2).copied();
            out.push(ALPHABET[(b0 >> 2) as usize] as char);
            out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
            out.push(match b1 {
                Some(b1) => {
                    ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char
                }
                None => '=',
            });
            out.push(match b2 {
                Some(b2) => ALPHABET[(b2 & 0x3f) as usize] as char,
                None => '=',
            });
        }
        out
    }

    fn decode(s: &str) -> Result<Vec<u8>, String> {
        fn value(c: u8) -> Result<u8, String> {
            match c {
                b'A'..=b'Z' => Ok(c - b'A'),
                b'a'..=b'z' => Ok(c - b'a' + 26),
                b'0'..=b'9' => Ok(c - b'0' + 52),
                b'+' => Ok(62),
                b'/' => Ok(63),
                _ => Err(format!("invalid base64 byte: {c}")),
            }
        }
        let s = s.as_bytes();
        if !s.len().is_multiple_of(4) {
            return Err("invalid base64 length".to_string());
        }
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        for chunk in s.chunks(4) {
            let pad = chunk.iter().filter(|&&c| c == b'=').count();
            let v0 = value(chunk[0])?;
            let v1 = value(chunk[1])?;
            out.push((v0 << 2) | (v1 >> 4));
            if chunk[2] != b'=' {
                let v2 = value(chunk[2])?;
                out.push((v1 << 4) | (v2 >> 2));
                if chunk[3] != b'=' {
                    let v3 = value(chunk[3])?;
                    out.push((v2 << 6) | v3);
                } else if pad != 1 {
                    return Err("invalid base64 padding".to_string());
                }
            } else if pad != 2 {
                return Err("invalid base64 padding".to_string());
            }
        }
        Ok(out)
    }

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        encode(bytes).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        decode(&s).map_err(serde::de::Error::custom)
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

#[cfg(test)]
mod base64_tests {
    use super::base64_bytes::*;

    #[test]
    fn roundtrips_arbitrary_bytes() {
        for len in 0..8 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 5) as u8).collect();
            let mut buf = Vec::new();
            let mut ser = serde_json::Serializer::new(&mut buf);
            serialize(&bytes, &mut ser).unwrap();
            let mut de = serde_json::Deserializer::from_slice(&buf);
            let round_tripped = deserialize(&mut de).unwrap();
            assert_eq!(bytes, round_tripped);
        }
    }
}
