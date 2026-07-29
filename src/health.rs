//! What the last sync actually did.
//!
//! `status` has to answer "did the last sync succeed?", not "does the target
//! path happen to exist?". That answer cannot be derived from the transactional
//! state file: a sync that fails writes no transaction at all, so the state
//! file is byte-for-byte what it was before the failure and every derived
//! check still looks green. This record is therefore written unconditionally,
//! outside the transaction, after every sync attempt — success, partial
//! success, conflict, or hard error.

use crate::paths;
use crate::util::{self, R};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncResult {
    /// Everything the config asked for was applied.
    Ok,
    /// Applied, but at least one feature was deliberately left alone.
    Degraded,
    /// Stopped on divergent edits that need an explicit human choice.
    Conflicts,
    /// Waiting for `--migrate` confirmation; nothing was written.
    Blocked,
    /// The run itself errored out.
    Failed,
}

impl SyncResult {
    pub fn label(self) -> &'static str {
        match self {
            SyncResult::Ok => "ok",
            SyncResult::Degraded => "degraded",
            SyncResult::Conflicts => "conflicts",
            SyncResult::Blocked => "blocked",
            SyncResult::Failed => "failed",
        }
    }

    pub fn healthy(self) -> bool {
        self == SyncResult::Ok
    }
}

/// One feature (optionally one named unit inside it) that the sync skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub feature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastSync {
    pub finished_ms: u128,
    pub result: SyncResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    #[serde(default)]
    pub conflicts: usize,
    #[serde(default)]
    pub applied: usize,
    #[serde(default)]
    pub skipped: Vec<Note>,
}

pub fn path() -> std::path::PathBuf {
    paths::store_state_dir().join("last-sync.json")
}

pub fn record(record: &LastSync) -> R<()> {
    util::ensure_private_dir(&paths::store_state_dir())?;
    util::write_private(
        &path(),
        &serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?,
    )
}

pub fn load() -> R<Option<LastSync>> {
    match util::read_to_string_opt(&path())? {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| util::ctx(&path(), e)),
        _ => Ok(None),
    }
}

/// `2026-07-29 10:44:12Z`, without pulling in a date crate.
pub fn format_epoch_ms(ms: u128) -> String {
    let secs = (ms / 1000) as i64;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let time = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Human-scale age, so a "green" status that is three weeks stale reads as one.
pub fn format_age(ms: u128) -> String {
    let now = util::now_millis();
    let seconds = now.saturating_sub(ms) / 1000;
    match seconds {
        0..=90 => "just now".to_string(),
        s if s < 5_400 => format!("{} minute(s) ago", s / 60),
        s if s < 172_800 => format!("{} hour(s) ago", s / 3600),
        s => format!("{} day(s) ago", s / 86_400),
    }
}

/// Howard Hinnant's `civil_from_days`, days since the Unix epoch to y/m/d.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_epochs() {
        assert_eq!(format_epoch_ms(0), "1970-01-01 00:00:00Z");
        assert_eq!(format_epoch_ms(1_769_000_000_000), "2026-01-21 12:53:20Z");
    }
}
