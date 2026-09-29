//! What an anonymization run changed. Reports never contain original
//! values, pseudonyms or the date offset.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

/// What happened to a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Replaced by a consistent pseudonym.
    Pseudonymized,
    /// Removed.
    Removed,
    /// Date shifted.
    Shifted,
    /// Free text replaced by `REDACTED`.
    Redacted,
}

/// Counts of changes at one location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Counts {
    /// Values replaced by pseudonyms.
    pub pseudonymized: u64,
    /// Values removed.
    pub removed: u64,
    /// Dates shifted.
    pub shifted: u64,
    /// Free texts redacted.
    pub redacted: u64,
}

/// The report of a run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Report {
    /// Messages or documents processed.
    pub messages: u64,
    /// Changes per location, such as `HL7 PID-5` or `ASTM P-8`.
    pub changes: BTreeMap<String, Counts>,
    /// Things a person should check, such as bytes that could not be
    /// processed.
    pub warnings: Vec<String>,
    /// Whether the pseudonym key was supplied (runs with the same key give
    /// the same pseudonyms) or random.
    pub key_supplied: bool,
    /// Whether the date shift was supplied or random.
    pub date_shift_supplied: bool,
}

impl Report {
    /// Records one change.
    pub fn note(&mut self, location: impl Into<String>, action: Action) {
        let counts = self.changes.entry(location.into()).or_default();
        match action {
            Action::Pseudonymized => counts.pseudonymized += 1,
            Action::Removed => counts.removed += 1,
            Action::Shifted => counts.shifted += 1,
            Action::Redacted => counts.redacted += 1,
        }
    }

    /// Records a warning.
    pub fn warn(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    /// Total changes.
    pub fn total(&self) -> u64 {
        self.changes
            .values()
            .map(|c| c.pseudonymized + c.removed + c.shifted + c.redacted)
            .sum()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} message(s), {} change(s)",
            self.messages,
            self.total()
        )?;
        for (location, counts) in &self.changes {
            let mut parts = Vec::new();
            for (count, label) in [
                (counts.pseudonymized, "pseudonymized"),
                (counts.removed, "removed"),
                (counts.shifted, "shifted"),
                (counts.redacted, "redacted"),
            ] {
                if count > 0 {
                    parts.push(format!("{count} {label}"));
                }
            }
            writeln!(f, "  {location}: {}", parts.join(", "))?;
        }
        writeln!(
            f,
            "pseudonym key: {}; date shift: {}",
            if self.key_supplied {
                "supplied"
            } else {
                "random (not reproducible)"
            },
            if self.date_shift_supplied {
                "supplied"
            } else {
                "random (not recorded)"
            }
        )?;
        for warning in &self.warnings {
            writeln!(f, "warning: {warning}")?;
        }
        Ok(())
    }
}
