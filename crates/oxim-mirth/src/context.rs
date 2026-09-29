//! Collecting report items while a channel is converted.

use crate::report::{Outcome, ReportItem};
use crate::templates::Globals;

/// Report items of one channel (or of the server-wide part of an export),
/// plus the configuration values connectors may refer to.
pub(crate) struct Notes<'a> {
    channel: Option<String>,
    items: &'a mut Vec<ReportItem>,
    globals: &'a Globals,
    /// Where this channel's items start.
    start: usize,
}

impl<'a> Notes<'a> {
    pub(crate) fn new(
        channel: Option<String>,
        items: &'a mut Vec<ReportItem>,
        globals: &'a Globals,
    ) -> Self {
        let start = items.len();
        Self {
            channel,
            items,
            globals,
            start,
        }
    }

    /// Adds an item before the other items of this channel.
    pub(crate) fn add_first(
        &mut self,
        outcome: Outcome,
        element: &str,
        location: &str,
        detail: impl Into<String>,
    ) {
        self.items.insert(
            self.start,
            ReportItem {
                channel: self.channel.clone(),
                element: element.to_owned(),
                location: location.to_owned(),
                outcome,
                detail: detail.into(),
            },
        );
    }

    pub(crate) fn add(
        &mut self,
        outcome: Outcome,
        element: &str,
        location: &str,
        detail: impl Into<String>,
    ) {
        self.items.push(ReportItem {
            channel: self.channel.clone(),
            element: element.to_owned(),
            location: location.to_owned(),
            outcome,
            detail: detail.into(),
        });
    }

    pub(crate) fn converted(&mut self, element: &str, location: &str, detail: impl Into<String>) {
        self.add(Outcome::Converted, element, location, detail);
    }

    pub(crate) fn approximated(
        &mut self,
        element: &str,
        location: &str,
        detail: impl Into<String>,
    ) {
        self.add(Outcome::Approximated, element, location, detail);
    }

    pub(crate) fn unsupported(&mut self, element: &str, location: &str, detail: impl Into<String>) {
        self.add(Outcome::Unsupported, element, location, detail);
    }

    /// Replaces `${name}` configuration placeholders, reporting the ones
    /// without a value.
    pub(crate) fn resolve(&mut self, text: &str, element: &str, location: &str) -> String {
        let (resolved, missing) = self.globals.resolve(text);
        if !missing.is_empty() {
            self.approximated(
                element,
                location,
                format!(
                    "{text:?} refers to Mirth variables without a value ({}); replace them in the \
                     channel file or pass values when importing",
                    missing.join(", ")
                ),
            );
        }
        resolved
    }

    /// The number of unsupported items reported so far for this channel.
    pub(crate) fn outcomes(&self, outcome: Outcome) -> usize {
        self.items
            .iter()
            .filter(|item| item.channel == self.channel && item.outcome == outcome)
            .count()
    }
}
