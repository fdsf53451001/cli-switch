//! Skills and instructions sync via symlinks to the canonical store.
//!
//! Symlinks make these bidirectional for free: editing through any CLI's path
//! edits the canonical file. We also "adopt" pre-existing real content into the
//! store on first run, and never clobber a conflicting real file without asking.

use crate::model::Cli;
use crate::paths;
use crate::util::{self, R};
use std::fs;
use std::path::Path;

#[derive(Default)]
pub struct LinkOutcome {
    pub linked: Vec<String>,
    pub adopted: Vec<String>,
    pub conflicts: Vec<String>,
}

impl LinkOutcome {
    #[allow(dead_code)]
    fn merge(&mut self, other: LinkOutcome) {
        self.linked.extend(other.linked);
        self.adopted.extend(other.adopted);
        self.conflicts.extend(other.conflicts);
    }
}

/// Sync instructions (canonical AGENTS.md -> each CLI's instructions file).
pub fn sync_instructions(clis: &[Cli]) -> R<LinkOutcome> {
    let canon = paths::store_instructions();
    let mut out = LinkOutcome::default();

    for &cli in clis {
        let link = paths::instructions_file(cli);
        adopt_then_link_file(&canon, &link, cli.id(), &mut out)?;
    }
    Ok(out)
}

/// Sync skills: adopt each CLI's real skill folders into the store, then symlink
/// every canonical skill folder back into each CLI's skills dir.
pub fn sync_skills(clis: &[Cli]) -> R<LinkOutcome> {
    let store = paths::store_skills();
    util::ensure_dir(&store)?;
    let mut out = LinkOutcome::default();

    // Phase 1: adopt real skill folders from each CLI into the store.
    for &cli in clis {
        let dir = paths::skills_dir(cli);
        let reserved = paths::skills_reserved(cli);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if reserved.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let meta = match path.symlink_metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Only adopt real directories that look like skills.
            if meta.file_type().is_symlink() || !meta.is_dir() {
                continue;
            }
            if !path.join("SKILL.md").exists() {
                continue;
            }
            let dest = store.join(&name);
            if dest.exists() {
                out.conflicts.push(format!(
                    "skill '{}' exists in store and in {} — left {}'s copy in place",
                    name,
                    cli.id(),
                    cli.id()
                ));
                continue;
            }
            move_dir(&path, &dest)?;
            out.adopted
                .push(format!("skill '{}' adopted from {}", name, cli.id()));
        }
    }

    // Phase 2: link every canonical skill into every CLI's skills dir.
    let canon_skills: Vec<String> = fs::read_dir(&store)
        .map_err(|e| util::ctx(&store, e))?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.'))
        .collect();

    for &cli in clis {
        let dir = paths::skills_dir(cli);
        util::ensure_dir(&dir)?;
        for name in &canon_skills {
            let target = store.join(name);
            let link = dir.join(name);
            link_dir(&target, &link, cli.id(), name, &mut out)?;
        }
    }

    Ok(out)
}

#[allow(dead_code)]
pub fn sync_all(clis: &[Cli]) -> R<LinkOutcome> {
    let mut out = sync_instructions(clis)?;
    out.merge(sync_skills(clis)?);
    Ok(out)
}

// ───────────────────────── file/dir linking ─────────────────────────

fn adopt_then_link_file(canon: &Path, link: &Path, cli: &str, out: &mut LinkOutcome) -> R<()> {
    let lmeta = link.symlink_metadata().ok();

    // Already a symlink? Re-point if needed, else done.
    if let Some(m) = &lmeta {
        if m.file_type().is_symlink() {
            if fs::read_link(link).ok().as_deref() == Some(canon) {
                return Ok(());
            }
            fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
            make_symlink(canon, link, false)?;
            out.linked.push(format!("{}: instructions relinked", cli));
            return Ok(());
        }
    }

    // Real file present.
    if lmeta.is_some() {
        let cli_content = fs::read_to_string(link).unwrap_or_default();
        let canon_content = fs::read_to_string(canon).unwrap_or_default();
        let canon_is_placeholder =
            canon_content.trim().is_empty() || canon_content.contains("single source of truth");
        if cli_content.trim().is_empty() {
            // Empty real file — safe to replace with a link.
            fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
        } else if canon_is_placeholder {
            // Adopt the CLI's content into the canonical store, then link.
            util::write_atomic(canon, &cli_content)?;
            fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
            out.adopted
                .push(format!("instructions adopted from {}", cli));
        } else if cli_content.trim() == canon_content.trim() {
            fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
        } else {
            out.conflicts.push(format!(
                "{}: instructions file has its own content — merge into {} then re-run",
                cli,
                canon.display()
            ));
            return Ok(());
        }
    }

    make_symlink(canon, link, false)?;
    out.linked.push(format!("{}: instructions linked", cli));
    Ok(())
}

fn link_dir(target: &Path, link: &Path, cli: &str, name: &str, out: &mut LinkOutcome) -> R<()> {
    if let Ok(m) = link.symlink_metadata() {
        if m.file_type().is_symlink() {
            if fs::read_link(link).ok().as_deref() == Some(target) {
                return Ok(());
            }
            fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
        } else {
            // Real dir already there (adopt conflict or duplicate) — don't clobber.
            out.conflicts.push(format!(
                "{}: real skill dir '{}' blocks link — resolve manually",
                cli, name
            ));
            return Ok(());
        }
    }
    make_symlink(target, link, true)?;
    out.linked.push(format!("{}: skill '{}' linked", cli, name));
    Ok(())
}

// ───────────────────────── cross-platform primitives ─────────────────────────

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path, _is_dir: bool) -> R<()> {
    util::ensure_parent(link)?;
    std::os::unix::fs::symlink(target, link).map_err(|e| util::ctx(link, e))
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path, is_dir: bool) -> R<()> {
    util::ensure_parent(link)?;
    let res = if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    if let Err(e) = res {
        // Windows often forbids symlinks without Developer Mode — fall back to copy.
        eprintln!(
            "  [!] symlink failed ({}); copied instead (bidirectional edits won't be live)",
            e
        );
        if is_dir {
            copy_dir_all(target, link)?;
        } else {
            util::ensure_parent(link)?;
            std::fs::copy(target, link).map_err(|er| util::ctx(link, er))?;
        }
    }
    Ok(())
}

fn move_dir(src: &Path, dest: &Path) -> R<()> {
    util::ensure_parent(dest)?;
    if fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    // Cross-device fallback.
    copy_dir_all(src, dest)?;
    fs::remove_dir_all(src).map_err(|e| util::ctx(src, e))
}

fn copy_dir_all(src: &Path, dest: &Path) -> R<()> {
    util::ensure_dir(dest)?;
    for entry in fs::read_dir(src).map_err(|e| util::ctx(src, e))? {
        let entry = entry.map_err(|e| util::ctx(src, e))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| util::ctx(&to, e))?;
        }
    }
    Ok(())
}
