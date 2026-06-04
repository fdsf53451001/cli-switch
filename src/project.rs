//! Project-local sync. This mirrors setup_agent_skill.sh:
//! AGENTS.md is the project source of truth, .agents/skills holds shared skills,
//! and CLI-specific project files point back to those canonical paths.

use crate::model::Cli;
use crate::util::{self, R};
use std::fs;
use std::path::Path;

pub struct Options {
    pub instructions: bool,
    pub skills: bool,
    pub dry_run: bool,
}

#[derive(Default)]
pub struct Outcome {
    pub actions: Vec<String>,
    pub conflicts: Vec<String>,
    pub notes: Vec<String>,
}

pub fn sync(clis: &[Cli], opts: &Options) -> R<Outcome> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let agents = root.join("AGENTS.md");
    let skills = root.join(".agents").join("skills");
    let rules = root.join(".agents").join("rules");
    let mut out = Outcome::default();

    if !agents.exists() {
        return Err(format!(
            "{} is required for project sync. Create it first, then rerun `agent-sync sync`.",
            agents.display()
        ));
    }

    if !opts.dry_run {
        util::ensure_dir(&skills)?;
        util::ensure_dir(&rules)?;
    } else {
        out.actions
            .push(format!("would ensure {}", skills.display()));
        out.actions
            .push(format!("would ensure {}", rules.display()));
    }

    if opts.instructions {
        sync_instructions(&root, &agents, clis, opts.dry_run, &mut out)?;
    }
    if opts.skills {
        sync_skills(&root, &skills, clis, opts.dry_run, &mut out)?;
    }

    Ok(out)
}

fn sync_instructions(
    root: &Path,
    agents: &Path,
    clis: &[Cli],
    dry_run: bool,
    out: &mut Outcome,
) -> R<()> {
    for &cli in clis {
        match cli {
            Cli::Claude => link_path(agents, &root.join("CLAUDE.md"), dry_run, out)?,
            Cli::Codex => out
                .notes
                .push("codex: project AGENTS.md is read directly".to_string()),
            Cli::Opencode => out
                .notes
                .push("opencode: project AGENTS.md is read directly".to_string()),
            Cli::Kiro => link_path(
                agents,
                &root.join(".kiro").join("steering").join("AGENTS.md"),
                dry_run,
                out,
            )?,
            Cli::Antigravity => write_antigravity_rule(root, dry_run, out)?,
        }
    }
    Ok(())
}

fn sync_skills(
    root: &Path,
    skills: &Path,
    clis: &[Cli],
    dry_run: bool,
    out: &mut Outcome,
) -> R<()> {
    for &cli in clis {
        match cli {
            Cli::Claude => link_path(skills, &root.join(".claude").join("skills"), dry_run, out)?,
            Cli::Kiro => link_path(skills, &root.join(".kiro").join("skills"), dry_run, out)?,
            Cli::Codex => out
                .notes
                .push("codex: project .agents/skills is used directly".to_string()),
            Cli::Opencode => out
                .notes
                .push("opencode: project .agents/skills is used directly".to_string()),
            Cli::Antigravity => out
                .notes
                .push("antigravity: project .agents/skills is used directly".to_string()),
        }
    }
    Ok(())
}

fn write_antigravity_rule(root: &Path, dry_run: bool, out: &mut Outcome) -> R<()> {
    let path = root.join(".agents").join("rules").join("agents-root.md");
    let body = r#"---
description: Root project instructions. Always applied.
activation: always
---

@/AGENTS.md
"#;

    if path.exists() {
        out.notes.push(format!(
            "antigravity: rule already exists at {}",
            path.display()
        ));
        return Ok(());
    }
    if dry_run {
        out.actions
            .push(format!("would create antigravity rule {}", path.display()));
        return Ok(());
    }

    util::write_atomic(&path, body)?;
    out.actions
        .push(format!("created antigravity rule {}", path.display()));
    Ok(())
}

fn link_path(target: &Path, link: &Path, dry_run: bool, out: &mut Outcome) -> R<()> {
    if let Ok(meta) = link.symlink_metadata() {
        if meta.file_type().is_symlink() {
            if fs::read_link(link).ok().as_deref() == Some(target) {
                out.notes.push(format!(
                    "already linked {} -> {}",
                    link.display(),
                    target.display()
                ));
                return Ok(());
            }
            if dry_run {
                out.actions.push(format!(
                    "would relink {} -> {}",
                    link.display(),
                    target.display()
                ));
                return Ok(());
            }
            fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
            make_symlink(target, link, target.is_dir())?;
            out.actions.push(format!(
                "relinked {} -> {}",
                link.display(),
                target.display()
            ));
            return Ok(());
        }

        out.conflicts.push(format!(
            "{} exists and is not a symlink; merge it into {} then rerun",
            link.display(),
            target.display()
        ));
        return Ok(());
    }

    if dry_run {
        out.actions.push(format!(
            "would link {} -> {}",
            link.display(),
            target.display()
        ));
        return Ok(());
    }
    make_symlink(target, link, target.is_dir())?;
    out.actions
        .push(format!("linked {} -> {}", link.display(), target.display()));
    Ok(())
}

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
        if is_dir {
            copy_dir_all(target, link)?;
        } else {
            fs::copy(target, link).map_err(|er| util::ctx(link, er))?;
        }
        eprintln!("  [!] symlink failed ({}); copied instead", e);
    }
    Ok(())
}

#[cfg(windows)]
fn copy_dir_all(src: &Path, dest: &Path) -> R<()> {
    util::ensure_dir(dest)?;
    for entry in fs::read_dir(src).map_err(|e| util::ctx(src, e))? {
        let entry = entry.map_err(|e| util::ctx(src, e))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| util::ctx(&to, e))?;
        }
    }
    Ok(())
}
