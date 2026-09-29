//! The migration report: what became of every element of the export.

use std::fmt::Write as _;

use serde::Serialize;

use crate::error::MirthError;

/// What became of one element of the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Converted with the same behavior.
    Converted,
    /// Converted with a difference the report explains.
    Approximated,
    /// Not converted; OXIM has no equivalent yet or the element must be
    /// rewritten by hand.
    Unsupported,
}

impl Outcome {
    /// The lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Converted => "converted",
            Self::Approximated => "approximated",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One element of the export and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportItem {
    /// The OXIM channel the element belongs to; `None` for server-wide
    /// elements such as global scripts.
    pub channel: Option<String>,
    /// A short description, for example `destination "To LIS" (TCP Sender)`.
    pub element: String,
    /// The element's location in the export, XPath style.
    pub location: String,
    /// What became of it.
    pub outcome: Outcome,
    /// What was done, or why it was not.
    pub detail: String,
}

/// One imported channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelSummary {
    /// The Mirth channel identifier.
    pub mirth_id: Option<String>,
    /// The Mirth channel name.
    pub mirth_name: String,
    /// The OXIM channel identifier.
    pub oxim_id: String,
    /// The file the channel was written to.
    pub file_name: String,
    /// Whether the channel is a draft that OXIM does not load, because its
    /// source connector has no OXIM equivalent.
    pub draft: bool,
}

/// The migration report of one import.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    /// What was imported, for example `channel export (Mirth Connect 3.12.0)`.
    pub export: String,
    /// The imported channels.
    pub channels: Vec<ChannelSummary>,
    /// Every element and what became of it.
    pub items: Vec<ReportItem>,
}

impl MigrationReport {
    /// How many items have `outcome`.
    pub fn count(&self, outcome: Outcome) -> usize {
        self.items
            .iter()
            .filter(|item| item.outcome == outcome)
            .count()
    }

    /// The items of one OXIM channel (`None`: server-wide items).
    pub fn items_for<'a>(
        &'a self,
        channel: Option<&'a str>,
    ) -> impl Iterator<Item = &'a ReportItem> + 'a {
        self.items
            .iter()
            .filter(move |item| item.channel.as_deref() == channel)
    }

    fn count_for(&self, channel: Option<&str>, outcome: Outcome) -> usize {
        self.items_for(channel)
            .filter(|item| item.outcome == outcome)
            .count()
    }

    /// The report as pretty-printed JSON.
    pub fn to_json(&self) -> Result<String, MirthError> {
        serde_json::to_string_pretty(self).map_err(|e| MirthError::Report(e.to_string()))
    }

    /// The report as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Mirth Connect migration report\n\n");
        let _ = writeln!(out, "Imported: {}\n", cell(&self.export));
        out.push_str("| Result | Items |\n|---|---|\n");
        for outcome in [
            Outcome::Converted,
            Outcome::Approximated,
            Outcome::Unsupported,
        ] {
            let _ = writeln!(out, "| {} | {} |", outcome.as_str(), self.count(outcome));
        }
        out.push('\n');
        out.push_str(
            "Converted elements behave as in Mirth Connect. Approximated elements were \
             converted with the difference described. Unsupported elements were left out \
             and need attention before the channel goes live.\n\n",
        );
        if !self.channels.is_empty() {
            out.push_str("## Channels\n\n");
            out.push_str(
                "| Mirth channel | OXIM channel | File | Converted | Approximated | Unsupported |\n\
                 |---|---|---|---|---|---|\n",
            );
            for channel in &self.channels {
                let id = Some(channel.oxim_id.as_str());
                let _ = writeln!(
                    out,
                    "| {} | `{}` | `{}`{} | {} | {} | {} |",
                    cell(&channel.mirth_name),
                    channel.oxim_id,
                    channel.file_name,
                    if channel.draft { " (draft)" } else { "" },
                    self.count_for(id, Outcome::Converted),
                    self.count_for(id, Outcome::Approximated),
                    self.count_for(id, Outcome::Unsupported),
                );
            }
            out.push('\n');
        }
        if self.items_for(None).next().is_some() {
            out.push_str("## Server-wide elements\n\n");
            write_items(&mut out, self.items_for(None));
        }
        for channel in &self.channels {
            let _ = writeln!(
                out,
                "## Channel `{}` (Mirth: {})\n",
                channel.oxim_id,
                cell(&channel.mirth_name)
            );
            if channel.draft {
                out.push_str(
                    "This channel was written as a draft (`.yaml.draft`) because its source \
                     connector has no OXIM equivalent yet. OXIM does not load it until the \
                     source is replaced and the file renamed to `.yaml`.\n\n",
                );
            }
            write_items(&mut out, self.items_for(Some(channel.oxim_id.as_str())));
        }
        out
    }
}

fn write_items<'a>(out: &mut String, items: impl Iterator<Item = &'a ReportItem>) {
    out.push_str("| Result | Element | Detail | Location |\n|---|---|---|---|\n");
    for item in items {
        let _ = writeln!(
            out,
            "| {} | {} | {} | `{}` |",
            item.outcome.as_str(),
            cell(&item.element),
            cell(&item.detail),
            item.location.replace('`', "'"),
        );
    }
    out.push('\n');
}

/// Text safe inside a Markdown table cell.
fn cell(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_markdown_and_json() {
        let report = MigrationReport {
            export: "channel export (Mirth Connect 3.12.0)".into(),
            channels: vec![ChannelSummary {
                mirth_id: Some("abc".into()),
                mirth_name: "ADT | in".into(),
                oxim_id: "adt-in".into(),
                file_name: "adt-in.yaml".into(),
                draft: false,
            }],
            items: vec![
                ReportItem {
                    channel: None,
                    element: "global deploy script".into(),
                    location: "/serverConfiguration/globalScripts/entry[1]".into(),
                    outcome: Outcome::Unsupported,
                    detail: "not converted".into(),
                },
                ReportItem {
                    channel: Some("adt-in".into()),
                    element: "source".into(),
                    location: "/channel/sourceConnector".into(),
                    outcome: Outcome::Converted,
                    detail: "mllp\nlistener".into(),
                },
            ],
        };
        let markdown = report.to_markdown();
        assert!(markdown.contains("| converted | 1 |"));
        assert!(markdown.contains("| ADT \\| in | `adt-in` | `adt-in.yaml` | 1 | 0 | 0 |"));
        assert!(markdown.contains("## Server-wide elements"));
        assert!(
            markdown
                .contains("| converted | source | mllp listener | `/channel/sourceConnector` |")
        );
        let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        assert_eq!(json["items"][0]["outcome"], "unsupported");
        assert_eq!(json["channels"][0]["oxim_id"], "adt-in");
    }
}
