//! Summaries of FHIR server responses.
//!
//! A transaction or batch returns a `transaction-response` /
//! `batch-response` Bundle with one entry per request, or an
//! OperationOutcome when the whole request failed. [`summarize_response`]
//! turns either into a [`ResponseSummary`] a channel can log or act on.

use crate::error::{FhirError, FhirResult};
use crate::resources::{Issue, Resource};

/// The outcome of one transaction or batch entry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EntryOutcome {
    /// The status line as returned, for example `201 Created`.
    pub status: String,
    /// The HTTP status code, when the status starts with one.
    pub code: Option<u16>,
    /// Location of the created or updated resource.
    pub location: Option<String>,
    /// Issues of the entry's OperationOutcome.
    pub issues: Vec<Issue>,
}

impl EntryOutcome {
    /// Whether the entry succeeded (a 2xx status).
    pub fn is_success(&self) -> bool {
        self.code.is_some_and(|code| (200..300).contains(&code))
    }
}

/// A summary of a server response.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResponseSummary {
    /// Whether every entry succeeded, or, for an OperationOutcome, whether
    /// it has no `error` or `fatal` issue.
    pub success: bool,
    /// Entry outcomes of a transaction or batch response, in order.
    pub entries: Vec<EntryOutcome>,
    /// Issues of a top-level OperationOutcome.
    pub issues: Vec<Issue>,
}

/// Summarizes a response body: a `transaction-response` or
/// `batch-response` Bundle, or an OperationOutcome.
pub fn summarize_response(body: &[u8]) -> FhirResult<ResponseSummary> {
    summarize(&Resource::from_json(body)?)
}

/// Summarizes a parsed response; see [`summarize_response`].
pub fn summarize(resource: &Resource) -> FhirResult<ResponseSummary> {
    match resource {
        Resource::Bundle(bundle)
            if matches!(
                bundle.kind.as_str(),
                "transaction-response" | "batch-response"
            ) =>
        {
            let entries: Vec<EntryOutcome> = bundle
                .entry
                .iter()
                .map(|entry| {
                    let response = entry.response.as_ref();
                    let status = response.map(|r| r.status.clone()).unwrap_or_default();
                    EntryOutcome {
                        code: status_code(&status),
                        location: response.and_then(|r| r.location.clone()),
                        issues: match response.and_then(|r| r.outcome.as_deref()) {
                            Some(Resource::OperationOutcome(outcome)) => outcome.issue.clone(),
                            _ => Vec::new(),
                        },
                        status,
                    }
                })
                .collect();
            Ok(ResponseSummary {
                success: entries.iter().all(EntryOutcome::is_success),
                entries,
                issues: Vec::new(),
            })
        }
        Resource::OperationOutcome(outcome) => Ok(ResponseSummary {
            success: !outcome.issue.iter().any(Issue::is_error),
            entries: Vec::new(),
            issues: outcome.issue.clone(),
        }),
        Resource::Bundle(bundle) => Err(FhirError::Unsupported(format!(
            "a {:?} bundle is not a transaction or batch response",
            bundle.kind
        ))),
        other => Err(FhirError::Unsupported(format!(
            "a {} is not a transaction response",
            other.resource_type()
        ))),
    }
}

/// The leading three-digit code of a status line.
fn status_code(status: &str) -> Option<u16> {
    let digits = status.trim_start().get(..3)?;
    if digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_a_transaction_response() {
        let summary = summarize_response(
            br#"{"resourceType":"Bundle","type":"transaction-response","entry":[
              {"response":{"status":"201 Created","location":"Observation/1/_history/1"}},
              {"response":{"status":"200 OK"}}
            ]}"#,
        )
        .unwrap();
        assert!(summary.success);
        assert_eq!(summary.entries.len(), 2);
        assert_eq!(summary.entries[0].code, Some(201));
        assert_eq!(
            summary.entries[0].location.as_deref(),
            Some("Observation/1/_history/1")
        );
    }

    #[test]
    fn reports_failed_entries_with_their_issues() {
        let summary = summarize_response(
            br#"{"resourceType":"Bundle","type":"batch-response","entry":[
              {"response":{"status":"201"}},
              {"response":{"status":"422 Unprocessable Entity","outcome":{"resourceType":"OperationOutcome",
                "issue":[{"severity":"error","code":"required","diagnostics":"Observation.code is required"}]}}}
            ]}"#,
        )
        .unwrap();
        assert!(!summary.success);
        assert!(summary.entries[0].is_success());
        assert_eq!(summary.entries[1].code, Some(422));
        assert_eq!(
            summary.entries[1].issues[0].diagnostics.as_deref(),
            Some("Observation.code is required")
        );
    }

    #[test]
    fn summarizes_an_operation_outcome() {
        let failed = summarize_response(
            br#"{"resourceType":"OperationOutcome","issue":[{"severity":"fatal","code":"exception"}]}"#,
        )
        .unwrap();
        assert!(!failed.success);
        assert_eq!(failed.issues.len(), 1);
        let informational = summarize_response(
            br#"{"resourceType":"OperationOutcome","issue":[{"severity":"information","code":"informational"}]}"#,
        )
        .unwrap();
        assert!(informational.success);
    }

    #[test]
    fn rejects_other_content() {
        assert!(summarize_response(br#"{"resourceType":"Bundle","type":"searchset"}"#).is_err());
        assert!(summarize_response(br#"{"resourceType":"Patient"}"#).is_err());
        assert!(summarize_response(b"not json").is_err());
        assert_eq!(status_code("abc"), None);
        assert_eq!(status_code("20"), None);
    }
}
