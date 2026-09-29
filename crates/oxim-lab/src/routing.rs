//! Test routing: which devices perform which tests.

use std::collections::{BTreeSet, HashMap};

use oxim_formats::{DelimitedDocument, DelimitedOptions};
use oxim_model::CodeableConcept;
use thiserror::Error;

/// Errors reading a routing table.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RoutingError {
    /// The text is not valid CSV.
    #[error("invalid CSV: {0}")]
    Csv(String),
    /// A column other than `test`, `device` and `note`.
    #[error("unknown column {0:?}; use test, device and note")]
    UnknownColumn(String),
    /// A required column is missing.
    #[error("missing column {0:?}")]
    MissingColumn(&'static str),
    /// A row leaves a required column empty.
    #[error("row {row}: {column:?} is empty")]
    EmptyValue {
        /// The 1-based data row.
        row: usize,
        /// The column.
        column: &'static str,
    },
}

/// Which devices perform which tests, read from a CSV table:
///
/// ```csv
/// test,device,note
/// GLU,chem-1,
/// GLU,chem-2,backup analyzer
/// HGB,hema-1,
/// ```
///
/// A test may be performed by several devices; each is offered the test
/// until one of them reports a result. A test matches a row when any of its
/// codings has the row's code, so both the LIS code and the device code
/// work after `map-observations`. Tests in no row are performed by no
/// device. A UTF-8 byte order mark and blank rows are accepted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Routing {
    devices: HashMap<String, BTreeSet<String>>,
}

impl Routing {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Routes `test` to `device`.
    pub fn with(mut self, test: &str, device: &str) -> Self {
        self.devices
            .entry(test.to_owned())
            .or_default()
            .insert(device.to_owned());
        self
    }

    /// Reads a table.
    pub fn from_csv(text: &str) -> Result<Self, RoutingError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let options = DelimitedOptions::csv().with_header(true);
        let document = DelimitedDocument::parse(text.as_bytes(), &options)
            .map_err(|e| RoutingError::Csv(e.to_string()))?;
        let mut test = None;
        let mut device = None;
        for (index, header) in document
            .headers()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            match header.trim().to_ascii_lowercase().as_str() {
                "test" => test = Some(index),
                "device" => device = Some(index),
                "note" | "" => {}
                other => return Err(RoutingError::UnknownColumn(other.to_owned())),
            }
        }
        let test = test.ok_or(RoutingError::MissingColumn("test"))?;
        let device = device.ok_or(RoutingError::MissingColumn("device"))?;
        let mut routing = Self::new();
        for row in 0..document.row_count() {
            let field = |column: usize| {
                document
                    .get(row, column)
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
            };
            let blank = (0..document.field_count(row).unwrap_or(0)).all(|c| field(c).is_none());
            if blank {
                continue;
            }
            let empty = |column| RoutingError::EmptyValue {
                row: row + 1,
                column,
            };
            let code = field(test).ok_or_else(|| empty("test"))?;
            let target = field(device).ok_or_else(|| empty("device"))?;
            routing = routing.with(&code, &target);
        }
        Ok(routing)
    }

    /// Whether the table has no rows.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// The devices that perform `test`.
    pub fn devices(&self, test: &CodeableConcept) -> BTreeSet<&str> {
        test.codings
            .iter()
            .filter_map(|coding| self.devices.get(&coding.code))
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// Whether `device` performs `test`.
    pub fn routes(&self, test: &CodeableConcept, device: &str) -> bool {
        test.codings.iter().any(|coding| {
            self.devices
                .get(&coding.code)
                .is_some_and(|devices| devices.contains(device))
        })
    }
}

#[cfg(test)]
mod tests {
    use oxim_model::Coding;

    use super::*;

    fn test(codes: &[&str]) -> CodeableConcept {
        CodeableConcept {
            codings: codes.iter().map(|code| Coding::new(*code)).collect(),
            text: None,
        }
    }

    #[test]
    fn reads_tables_with_several_devices_per_test() {
        let routing = Routing::from_csv(
            "\u{feff}Test,Device,Note\nGLU,chem-1,\nGLU,chem-2,backup\n,,\nHGB,hema-1,\n",
        )
        .unwrap();
        assert_eq!(
            routing
                .devices(&test(&["GLU"]))
                .into_iter()
                .collect::<Vec<_>>(),
            ["chem-1", "chem-2"]
        );
        // Any coding matches: the LIS code or the device code.
        assert!(routing.routes(&test(&["2345-7", "HGB"]), "hema-1"));
        assert!(!routing.routes(&test(&["HGB"]), "chem-1"));
        assert!(routing.devices(&test(&["NA"])).is_empty());
    }

    #[test]
    fn rejects_malformed_tables() {
        assert_eq!(
            Routing::from_csv("test,analyzer\nGLU,chem\n"),
            Err(RoutingError::UnknownColumn("analyzer".into()))
        );
        assert_eq!(
            Routing::from_csv("test\nGLU\n"),
            Err(RoutingError::MissingColumn("device"))
        );
        assert_eq!(
            Routing::from_csv("test,device\nGLU,\n"),
            Err(RoutingError::EmptyValue {
                row: 1,
                column: "device"
            })
        );
    }
}
