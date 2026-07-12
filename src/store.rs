//! The canonical store: the single source of truth plus per-CLI snapshots
//! (what each CLI's MCP set looked like at the end of the last sync). Snapshots
//! are what make the merge three-way: base = snapshot, theirs = current native,
//! ours = canonical.

use crate::model::Canonical;
use crate::paths;
use crate::util::{self, R};

pub fn load_canonical() -> R<Canonical> {
    match util::read_to_string_opt(&paths::store_mcp())? {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(&t).map_err(|e| util::ctx(&paths::store_mcp(), e))
        }
        _ => Ok(Canonical::default()),
    }
}

pub fn save_canonical(c: &Canonical) -> R<()> {
    let s = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
    util::write_private(&paths::store_mcp(), s.as_bytes())
}

/// Create the store skeleton on first use.
pub fn ensure_scaffold() -> R<()> {
    util::ensure_private_dir(&paths::store_root())?;
    util::ensure_dir(&paths::store_skills())?;
    util::ensure_private_dir(&paths::store_state_dir())?;
    if util::read_to_string_opt(&paths::store_instructions())?.is_none() {
        util::write_atomic(
            &paths::store_instructions(),
            "# Shared agent instructions\n\nEdit this file; it is the single source of truth synced to every CLI.\n",
        )?;
    }
    if util::read_to_string_opt(&paths::store_mcp())?.is_none() {
        save_canonical(&Canonical::default())?;
    }
    Ok(())
}
