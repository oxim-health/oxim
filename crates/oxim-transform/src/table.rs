//! Code tables: translations such as analyzer code `GLU` → LIS code `1520`.

use std::collections::HashMap;

use oxim_formats::{DelimitedDocument, DelimitedOptions};
use thiserror::Error;

/// Returned when a code table cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CodeTableError {
    /// The CSV text is malformed.
    #[error("invalid CSV: {0}")]
    Csv(String),
    /// A required column is missing from the header.
    #[error("the header has no {0:?} column")]
    MissingColumn(&'static str),
    /// The header names a column that is not understood.
    #[error("unknown column {0:?}; use from, to, display, system and context")]
    UnknownColumn(String),
    /// A row has an empty `from` or `to` value.
    #[error("row {row}: {column:?} must not be empty")]
    EmptyValue {
        /// 1-based data row number.
        row: usize,
        /// The column.
        column: &'static str,
    },
    /// Two rows translate the same code in the same context.
    #[error("row {row}: code {code:?} is already defined{}", context.as_deref().map(|c| format!(" for context {c:?}")).unwrap_or_default())]
    Duplicate {
        /// 1-based data row number of the second definition.
        row: usize,
        /// The code.
        code: String,
        /// The context, if the row has one.
        context: Option<String>,
    },
}

/// The translation of one code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEntry {
    /// The target code.
    pub to: String,
    /// Display text for the target code.
    pub display: Option<String>,
    /// The code system of the target code.
    pub system: Option<String>,
}

/// A code table read from CSV.
///
/// The header row names the columns, in any order: `from` and `to` are
/// required, `display`, `system` and `context` are optional.
///
/// ```csv
/// from,to,display,system,context
/// GLU,1520,Glucose,urn:oxim:lis,
/// GLU,1521,Glucose (POCT),urn:oxim:lis,poct-1
/// ```
///
/// A row with a `context` applies only when the lookup names that context
/// (for example a device or channel identifier); rows without a context
/// apply everywhere and are used when no context-specific row matches.
/// Defining the same code twice for the same context is an error. Rows whose
/// fields are all empty are ignored. A UTF-8 byte order mark is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodeTable {
    entries: HashMap<(String, String), CodeEntry>,
    case_insensitive: bool,
}

impl CodeTable {
    /// Reads a table with case-sensitive codes.
    pub fn from_csv(text: &str) -> Result<Self, CodeTableError> {
        Self::from_csv_with(text, false)
    }

    /// Reads a table. With `case_insensitive`, codes and contexts match
    /// regardless of letter case.
    pub fn from_csv_with(text: &str, case_insensitive: bool) -> Result<Self, CodeTableError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let options = DelimitedOptions::csv().with_header(true);
        let document = DelimitedDocument::parse(text.as_bytes(), &options)
            .map_err(|e| CodeTableError::Csv(e.to_string()))?;
        let headers: Vec<String> = document
            .headers()
            .unwrap_or_default()
            .into_iter()
            .map(|header| header.trim().to_ascii_lowercase())
            .collect();
        let mut columns: HashMap<&'static str, usize> = HashMap::new();
        for (index, header) in headers.iter().enumerate() {
            let name = match header.as_str() {
                "from" => "from",
                "to" => "to",
                "display" => "display",
                "system" => "system",
                "context" => "context",
                "" => continue,
                other => return Err(CodeTableError::UnknownColumn(other.to_owned())),
            };
            columns.insert(name, index);
        }
        let from = *columns
            .get("from")
            .ok_or(CodeTableError::MissingColumn("from"))?;
        let to = *columns
            .get("to")
            .ok_or(CodeTableError::MissingColumn("to"))?;
        let mut table = Self {
            entries: HashMap::new(),
            case_insensitive,
        };
        for row in 0..document.row_count() {
            let field = |column: Option<&usize>| -> Option<String> {
                column
                    .and_then(|&column| document.get(row, column))
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
            };
            let values: Vec<Option<String>> = (0..document.field_count(row).unwrap_or(0))
                .map(|column| field(Some(&column)))
                .collect();
            if values.iter().all(Option::is_none) {
                continue;
            }
            let number = row + 1;
            let code = field(Some(&from)).ok_or(CodeTableError::EmptyValue {
                row: number,
                column: "from",
            })?;
            let target = field(Some(&to)).ok_or(CodeTableError::EmptyValue {
                row: number,
                column: "to",
            })?;
            let context = field(columns.get("context"));
            let key = (
                table.fold(context.as_deref().unwrap_or_default()),
                table.fold(&code),
            );
            if table.entries.contains_key(&key) {
                return Err(CodeTableError::Duplicate {
                    row: number,
                    code,
                    context,
                });
            }
            table.entries.insert(
                key,
                CodeEntry {
                    to: target,
                    display: field(columns.get("display")),
                    system: field(columns.get("system")),
                },
            );
        }
        Ok(table)
    }

    fn fold(&self, text: &str) -> String {
        if self.case_insensitive {
            text.to_lowercase()
        } else {
            text.to_owned()
        }
    }

    /// Looks up `code`, preferring a row for `context` over a row without
    /// context.
    pub fn lookup(&self, context: Option<&str>, code: &str) -> Option<&CodeEntry> {
        let code = self.fold(code.trim());
        context
            .filter(|context| !context.is_empty())
            .and_then(|context| self.entries.get(&(self.fold(context), code.clone())))
            .or_else(|| self.entries.get(&(String::new(), code)))
    }

    /// The number of translations.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table has no translations.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tables_with_contexts() {
        let table = CodeTable::from_csv(
            "\u{feff}context,from,to,display,system\r\n,GLU,1520,Glucose,urn:lis\r\npoct-1,GLU,1521,\"Glucose, POCT\",urn:lis\r\n,,,,\r\n,HGB,2010,,\r\n",
        )
        .unwrap();
        assert_eq!(table.len(), 3);
        assert_eq!(table.lookup(None, "GLU").unwrap().to, "1520");
        assert_eq!(
            table
                .lookup(Some("poct-1"), "GLU")
                .unwrap()
                .display
                .as_deref(),
            Some("Glucose, POCT")
        );
        assert_eq!(table.lookup(Some("other"), "GLU").unwrap().to, "1520");
        assert_eq!(table.lookup(Some("poct-1"), " HGB ").unwrap().to, "2010");
        assert!(table.lookup(None, "glu").is_none());
        let insensitive = CodeTable::from_csv_with("from,to\nGlu,1520\n", true).unwrap();
        assert_eq!(insensitive.lookup(None, "GLU").unwrap().to, "1520");
    }

    #[test]
    fn reports_table_errors() {
        assert_eq!(
            CodeTable::from_csv("code,to\nA,B\n"),
            Err(CodeTableError::UnknownColumn("code".into()))
        );
        assert_eq!(
            CodeTable::from_csv("from,display\nA,B\n"),
            Err(CodeTableError::MissingColumn("to"))
        );
        assert_eq!(
            CodeTable::from_csv("from,to\nA,\n"),
            Err(CodeTableError::EmptyValue {
                row: 1,
                column: "to"
            })
        );
        assert!(matches!(
            CodeTable::from_csv("from,to\nA,1\nB,2\nA,3\n"),
            Err(CodeTableError::Duplicate { row: 3, .. })
        ));
        assert!(matches!(
            CodeTable::from_csv_with("from,to\nA,1\na,2\n", true),
            Err(CodeTableError::Duplicate { row: 2, .. })
        ));
        assert!(CodeTable::from_csv("from,to\n").unwrap().is_empty());
    }
}
