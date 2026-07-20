//! Project-local sync. This mirrors setup_agent_skill.sh:
//! AGENTS.md is the project source of truth, .agents/skills holds shared skills,
//! and CLI-specific project files point back to those canonical paths via
//! relative symlinks so the project tree stays portable.

use crate::model::Cli;
use crate::util::{self, R};
use std::fs;
use std::path::{Component, Path, PathBuf};

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

    if !opts.dry_run {
        ensure_agents_file(&agents, &mut out)?;
        util::ensure_dir(&skills)?;
        util::ensure_dir(&rules)?;
    } else {
        if !agents.exists() {
            out.actions
                .push(format!("would create {}", agents.display()));
        }
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

    if !opts.dry_run {
        ensure_gitignore(&root, clis, &mut out)?;
    } else {
        out.actions.push(format!(
            "would maintain {}",
            root.join(".gitignore").display()
        ));
    }

    Ok(out)
}

pub fn leave() -> R<()> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let agents = root.join("AGENTS.md");
    let skills = root.join(".agents").join("skills");
    remove_symlink_if_target(&root.join("CLAUDE.md"), &agents)?;
    remove_symlink_if_target(
        &root.join(".kiro").join("steering").join("AGENTS.md"),
        &agents,
    )?;
    remove_symlink_if_target(&root.join(".claude").join("skills"), &skills)?;
    remove_symlink_if_target(&root.join(".kiro").join("skills"), &skills)?;

    let antigravity_rule = root.join(".agents").join("rules").join("agents-root.md");
    if fs::read_to_string(&antigravity_rule)
        .map(|text| text.contains("@/AGENTS.md"))
        .unwrap_or(false)
    {
        fs::remove_file(&antigravity_rule).map_err(|e| util::ctx(&antigravity_rule, e))?;
    }
    Ok(())
}

fn remove_symlink_if_target(link: &Path, target: &Path) -> R<()> {
    let Ok(meta) = link.symlink_metadata() else {
        return Ok(());
    };
    if meta.file_type().is_symlink()
        && fs::read_link(link)
            .map(|current| same_link_target(&current, link, target))
            .unwrap_or(false)
    {
        fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
    }
    Ok(())
}

/// Compare an existing symlink's stored target against the canonical target.
/// Accepts both relative and absolute spellings so a symlink created by an
/// older absolute-path release is still recognized during `leave` and on a
/// later idempotent sync. Comparison uses lexical normalization of both paths
/// and, when the lexical comparison is inconclusive (e.g. one path goes
/// through `/tmp` and the other through `/private/tmp` on macOS), falls back
/// to a filesystem-resolved comparison via canonicalize.
fn same_link_target(stored: &Path, link: &Path, target: &Path) -> bool {
    let link_parent = link.parent().unwrap_or_else(|| Path::new(""));
    let stored_abs = if stored.is_absolute() {
        stored.to_path_buf()
    } else {
        link_parent.join(stored)
    };
    let expected_rel = relative_target(target, link).unwrap_or_else(|| target.to_path_buf());
    let expected_abs = if expected_rel.is_absolute() {
        expected_rel.clone()
    } else {
        link_parent.join(&expected_rel)
    };

    if stored == expected_rel {
        return true;
    }
    if stored == target {
        return true;
    }
    if normalize(&stored_abs) == normalize(&expected_abs) {
        return true;
    }
    // Lexical comparison disagrees (likely a `/tmp` vs `/private/tmp`-style
    // alias on macOS). Trust the filesystem.
    fs::canonicalize(&stored_abs).ok() == fs::canonicalize(&expected_abs).ok()
}

/// Relative path from `link.parent()` to `target`. Falls back to the absolute
/// target when the two have no common prefix (e.g. different Windows drives).
fn relative_target(target: &Path, link: &Path) -> Option<PathBuf> {
    let base = link.parent()?;
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    };
    let mut t_comps = target.components().collect::<Vec<_>>();
    let mut b_comps = base.components().collect::<Vec<_>>();
    if t_comps.first() != b_comps.first() {
        return Some(target);
    }
    while !t_comps.is_empty() && !b_comps.is_empty() && t_comps[0] == b_comps[0] {
        t_comps.remove(0);
        b_comps.remove(0);
    }
    let mut out = PathBuf::new();
    for _ in 0..b_comps.len() {
        out.push("..");
    }
    for c in t_comps {
        match c {
            Component::ParentDir => out.push(".."),
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn ensure_agents_file(path: &Path, out: &mut Outcome) -> R<()> {
    if path.exists() {
        return Ok(());
    }
    util::write_atomic(
        path,
        "# Project agent instructions\n\nAdd shared instructions for AI coding agents in this project.\n",
    )?;
    out.actions.push(format!("created {}", path.display()));
    Ok(())
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
            Cli::Copilot => out
                .notes
                .push("copilot: project AGENTS.md is read directly".to_string()),
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
            Cli::Copilot => out
                .notes
                .push("copilot: project .agents/skills is used directly".to_string()),
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
            let stored = fs::read_link(link).map_err(|e| util::ctx(link, e))?;
            let points_to_target = same_link_target(&stored, link, target);
            let expected_rel = relative_target(target, link);
            let stored_matches_relative =
                expected_rel.as_ref().map(|r| r == &stored).unwrap_or(false);

            if points_to_target && stored_matches_relative {
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
            make_relative_symlink(target, link, target.is_dir())?;
            out.actions.push(format!(
                "relinked {} -> {}",
                link.display(),
                target.display()
            ));
            return Ok(());
        }

        if meta.is_file() {
            if dry_run {
                out.actions.push(format!(
                    "would merge existing file {} into {} and replace with symlink",
                    link.display(),
                    target.display()
                ));
                return Ok(());
            }
            return merge_file_then_link(target, link, out);
        }

        out.conflicts.push(format!(
            "{} exists and is not a symlink or regular file; merge it into {} then rerun",
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
    make_relative_symlink(target, link, target.is_dir())?;
    out.actions
        .push(format!("linked {} -> {}", link.display(), target.display()));
    Ok(())
}

/// Line-based union merge of an existing `link` file with `target` (source of
/// truth). On success, `target` is updated with the merged content and `link`
/// is replaced with a symlink to `target`. On an unresolvable conflict the
/// files are left untouched and a conflict note is recorded.
fn merge_file_then_link(target: &Path, link: &Path, out: &mut Outcome) -> R<()> {
    let target_text = util::read_to_string_opt(target)?.unwrap_or_default();
    let link_text = util::read_to_string_opt(link)?.unwrap_or_default();

    if link_text == target_text {
        fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
        make_relative_symlink(target, link, false)?;
        out.actions.push(format!(
            "merged identical {} into {} and replaced with symlink",
            link.display(),
            target.display()
        ));
        return Ok(());
    }

    let target_lines: Vec<&str> = target_text.lines().collect();
    let link_lines: Vec<&str> = link_text.lines().collect();

    let Some(m) = union_line_merge(&target_lines, &link_lines) else {
        out.conflicts.push(format!(
            "{} differs from {} in a way that cannot be auto-merged; resolve manually then rerun",
            link.display(),
            target.display()
        ));
        return Ok(());
    };

    let trailing_nl_target = target_text.ends_with('\n');
    let trailing_nl_link = link_text.ends_with('\n');
    let trailing_nl = trailing_nl_target || trailing_nl_link;

    let mut merged = m.join("\n");
    if trailing_nl {
        merged.push('\n');
    }

    util::write_atomic(target, &merged)?;
    fs::remove_file(link).map_err(|e| util::ctx(link, e))?;
    make_relative_symlink(target, link, false)?;
    out.actions.push(format!(
        "merged existing {} into {} and replaced with symlink",
        link.display(),
        target.display()
    ));
    Ok(())
}

/// Two-way union line merge. Returns `None` only when the two inputs share no
/// common anchor line, which makes any interleaving ambiguous. Otherwise
/// returns the interleaving that preserves order from both sides, preferring
/// `target` lines first when a stable run of equal lines is encountered.
fn union_line_merge<'a>(a: &[&'a str], b: &[&'a str]) -> Option<Vec<&'a str>> {
    // LCS-based shortest common supersequence of lines. Each line participates
    // once; lines unique to one side keep their relative order; lines present
    // on both sides collapse to a single occurrence in the shared order.
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return Some(b.to_vec());
    }
    if n == 0 {
        return Some(a.to_vec());
    }

    // dp[i][j] = length of LCS for a[i..] and b[j..]
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    if dp[0][0] == 0 {
        // No common line: order is ambiguous, refuse to merge.
        return None;
    }

    let mut out: Vec<&str> = Vec::with_capacity(m + n);
    let (mut i, mut j) = (0, 0);
    while i < m && j < n {
        if a[i] == b[j] {
            out.push(a[i]);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    while i < m {
        out.push(a[i]);
        i += 1;
    }
    while j < n {
        out.push(b[j]);
        j += 1;
    }
    Some(out)
}

/// Keep `.gitignore` in sync with cli-switch's per-CLI project fixtures.
/// Adds entries for the CLI-private directories of every active CLI (always
/// including `.cli-switch/`). Idempotent: never duplicates existing entries,
/// never removes unrelated lines. The canonical `.agents/`, `AGENTS.md`, and
/// `CLAUDE.md` are deliberately left alone — they are the shared source of
/// truth and belong in version control.
fn ensure_gitignore(root: &Path, clis: &[Cli], out: &mut Outcome) -> R<()> {
    let mut entries: Vec<&'static str> = Vec::new();
    for &cli in clis {
        match cli {
            Cli::Claude => entries.push(".claude/"),
            Cli::Kiro => entries.push(".kiro/"),
            _ => {}
        }
    }
    entries.push(".cli-switch/");
    entries.sort();
    entries.dedup();

    let path = root.join(".gitignore");
    let existing = util::read_to_string_opt(&path)?;
    let raw_present: Vec<&str> = existing
        .as_deref()
        .map(|s| s.lines().map(|l| l.trim()).collect())
        .unwrap_or_default();
    let present: std::collections::BTreeSet<&str> = raw_present.iter().copied().collect();

    let mut to_add: Vec<&'static str> = entries
        .iter()
        .copied()
        .filter(|e| !present.contains(*e))
        .collect();
    if to_add.is_empty() {
        out.notes.push(format!(
            "{} already covers cli-switch entries",
            path.display()
        ));
        return Ok(());
    }
    to_add.sort();

    let mut body = existing.unwrap_or_default();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    if !body.is_empty() && !body.ends_with("\n\n") {
        body.push('\n');
    }
    body.push_str("# cli-switch managed entries (per-CLI private fixtures)\n");
    for entry in &to_add {
        body.push_str(entry);
        body.push('\n');
    }

    util::write_atomic(&path, &body)?;
    out.actions
        .push(format!("added {} to {}", to_add.join(", "), path.display()));
    Ok(())
}

#[cfg(unix)]
fn make_relative_symlink(target: &Path, link: &Path, _is_dir: bool) -> R<()> {
    util::ensure_parent(link)?;
    let rel = relative_target(target, link).unwrap_or_else(|| target.to_path_buf());
    std::os::unix::fs::symlink(&rel, link).map_err(|e| util::ctx(link, e))
}

#[cfg(windows)]
fn make_relative_symlink(target: &Path, link: &Path, is_dir: bool) -> R<()> {
    util::ensure_parent(link)?;
    let rel = relative_target(target, link).unwrap_or_else(|| target.to_path_buf());
    let res = if is_dir {
        std::os::windows::fs::symlink_dir(&rel, link)
    } else {
        std::os::windows::fs::symlink_file(&rel, link)
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
