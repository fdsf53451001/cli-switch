//! Project mappings are planned without writes, then applied through the shared journal.
use crate::engine::{self, InputGuard, Node, Operation};
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
    pub transaction: Option<String>,
}

pub fn sync(clis: &[Cli], opts: &Options) -> R<Outcome> {
    if clis.is_empty() || (!opts.instructions && !opts.skills) {
        return Ok(Outcome::default());
    }
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let agents = root.join("AGENTS.md");
    let skills = root.join(".agents/skills");
    let rule = root.join(".agents/rules/agents-root.md");
    let mut inputs = vec![root.join(".gitignore"), crate::paths::project_config()];
    if opts.instructions {
        inputs.push(agents.clone());
        if clis.contains(&Cli::Antigravity) {
            inputs.push(rule.clone());
        }
    }
    if opts.skills {
        inputs.push(skills.clone());
    }
    let mut links = Vec::new();
    for cli in clis {
        // Claude Code, Codex, opencode and Copilot read AGENTS.md natively.
        if opts.instructions && *cli == Cli::Kiro {
            links.push((agents.clone(), root.join(".kiro/steering/AGENTS.md")));
        }
        if opts.skills {
            match cli {
                Cli::Claude => links.push((skills.clone(), root.join(".claude/skills"))),
                Cli::Kiro => links.push((skills.clone(), root.join(".kiro/skills"))),
                _ => {}
            }
        }
    }
    // Older releases linked CLAUDE.md -> AGENTS.md. Claude now loads AGENTS.md
    // itself, so a leftover link would feed it the same instructions twice.
    let legacy_claude = root.join("CLAUDE.md");
    let retire_claude_link = opts.instructions
        && clis.contains(&Cli::Claude)
        && fs::symlink_metadata(&legacy_claude).is_ok_and(|m| m.file_type().is_symlink())
        && fs::read_link(&legacy_claude)
            .is_ok_and(|stored| same_link_target(&stored, &legacy_claude, &agents));
    if retire_claude_link {
        inputs.push(legacy_claude.clone());
    }
    inputs.extend(links.iter().map(|(_, link)| link.clone()));
    let guard = InputGuard::capture(inputs);
    let mut out = Outcome::default();
    let mut ops = Vec::new();
    let canonical = if opts.instructions {
        util::read_to_string_opt(&agents)?
    } else {
        None
    };
    // A missing canonical file can adopt ONE identical native value. Never
    // combine different instructions, even when they share headings or blanks.
    let mut adopted = canonical.clone();
    if opts.instructions && adopted.is_none() {
        for (target, link) in &links {
            if target != &agents
                || fs::symlink_metadata(link)
                    .map(|m| !m.is_file())
                    .unwrap_or(true)
            {
                continue;
            }
            if let Some(text) = util::read_to_string_opt(link)? {
                match &adopted {
                    Some(previous) if previous != &text => out.conflicts.push(format!("{} differs from another native instruction file; choose the content for {} manually", link.display(), agents.display())),
                    None => adopted = Some(text),
                    _ => {}
                }
            }
        }
    }
    if opts.instructions && canonical.is_none() {
        let text = adopted.clone().unwrap_or_else(|| "# Project agent instructions\n\nAdd shared instructions for AI coding agents in this project.\n".into());
        ops.push(Operation {
            path: agents.clone(),
            after: Node::File(text.into_bytes()),
            label: format!("create {}", agents.display()),
        });
    }
    if opts.skills && !skills.exists() {
        ops.push(Operation {
            path: skills.clone(),
            after: Node::Dir(Default::default()),
            label: format!("create {}", skills.display()),
        });
    }
    for (target, link) in links {
        let desired = relative_target(&target, &link).unwrap_or_else(|| target.clone());
        match fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let stored = fs::read_link(&link).map_err(|e| util::ctx(&link, e))?;
                if !same_link_target(&stored, &link, &target) {
                    out.conflicts.push(format!(
                        "{} points to {}; choose its target manually before syncing",
                        link.display(),
                        stored.display()
                    ));
                    continue;
                }
                if stored == desired {
                    out.notes.push(format!(
                        "already linked {} -> {}",
                        link.display(),
                        target.display()
                    ));
                    continue;
                }
            }
            Ok(meta) if meta.is_file() && target == agents => {
                let native = util::read_to_string_opt(&link)?;
                if native != adopted {
                    out.conflicts.push(format!("{} differs from {}; instructions cannot be auto-merged; reconcile the files manually then rerun", link.display(), target.display()));
                    continue;
                }
            }
            Ok(_) => {
                out.conflicts.push(format!("{} exists and is not a managed symlink or identical instruction file; merge it into {} manually then rerun", link.display(), target.display()));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(util::ctx(&link, e)),
        }
        let verb = if fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
            "relinked"
        } else {
            "linked"
        };
        ops.push(Operation {
            path: link.clone(),
            after: Node::Symlink(desired),
            label: format!("{verb} {} -> {}", link.display(), target.display()),
        });
    }
    if retire_claude_link {
        ops.push(Operation {
            path: legacy_claude.clone(),
            after: Node::Absent,
            label: format!(
                "removed legacy {} (Claude reads AGENTS.md directly)",
                legacy_claude.display()
            ),
        });
    }
    if opts.instructions && clis.contains(&Cli::Antigravity) && !rule.exists() {
        ops.push(Operation { path: rule, after: Node::File(b"---\ndescription: Root project instructions. Always applied.\nactivation: always\n---\n\n@/AGENTS.md\n".to_vec()), label: "create antigravity rule".into() });
    }
    if opts.instructions || opts.skills {
        plan_gitignore(&root, clis, &mut out, &mut ops)?;
    }
    if !out.conflicts.is_empty() {
        out.actions.clear();
        return Ok(out);
    }
    // Check every destination before the first write, including invalid parents.
    for op in &ops {
        engine::check_destination(&op.path)?;
    }
    out.actions = ops.iter().map(|op| op.label.clone()).collect();
    if !opts.dry_run && !ops.is_empty() {
        out.transaction = Some(engine::apply_project_operations(&ops, &guard)?);
    }
    Ok(out)
}

pub fn leave() -> R<()> {
    let _lock = util::acquire_lock(&crate::paths::store_root().join(".lock"), 120)?
        .ok_or("another cli-switch operation is in progress")?;
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
    matches!((fs::canonicalize(&stored_abs), fs::canonicalize(&expected_abs)), (Ok(a), Ok(b)) if a == b)
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

fn plan_gitignore(root: &Path, clis: &[Cli], out: &mut Outcome, ops: &mut Vec<Operation>) -> R<()> {
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

    ops.push(Operation {
        path: path.clone(),
        after: Node::File(body.into_bytes()),
        label: "project gitignore".into(),
    });
    out.actions
        .push(format!("added {} to {}", to_add.join(", "), path.display()));
    Ok(())
}
