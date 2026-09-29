//! Structural validation of the modeled resources.
//!
//! Checks required elements, the cardinality of modeled elements and the
//! value sets of the status codes OXIM emits. It is not a profile
//! validator: terminology bindings beyond those value sets, invariants and
//! profiles are left to a FHIR server or a dedicated validator.

use crate::datatypes::{Annotation, CodeableConcept, Quantity};
use crate::resources::Issue;
use crate::resources::{
    Bundle, DiagnosticReport, Observation, OperationOutcome, Patient, Resource, ServiceRequest,
    Specimen,
};

const OBSERVATION_STATUS: &[&str] = &[
    "registered",
    "preliminary",
    "final",
    "amended",
    "corrected",
    "cancelled",
    "entered-in-error",
    "unknown",
];
const REPORT_STATUS: &[&str] = &[
    "registered",
    "partial",
    "preliminary",
    "final",
    "amended",
    "corrected",
    "appended",
    "cancelled",
    "entered-in-error",
    "unknown",
];
const REQUEST_STATUS: &[&str] = &[
    "draft",
    "active",
    "on-hold",
    "revoked",
    "completed",
    "entered-in-error",
    "unknown",
];
const REQUEST_INTENT: &[&str] = &[
    "proposal",
    "plan",
    "directive",
    "order",
    "original-order",
    "reflex-order",
    "filler-order",
    "instance-order",
    "option",
];
const PRIORITY: &[&str] = &["routine", "urgent", "asap", "stat"];
const GENDER: &[&str] = &["male", "female", "other", "unknown"];
const SPECIMEN_STATUS: &[&str] = &[
    "available",
    "unavailable",
    "unsatisfactory",
    "entered-in-error",
];
const BUNDLE_TYPE: &[&str] = &[
    "document",
    "message",
    "transaction",
    "transaction-response",
    "batch",
    "batch-response",
    "history",
    "searchset",
    "collection",
];
const HTTP_METHOD: &[&str] = &["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"];
const COMPARATOR: &[&str] = &["<", "<=", ">=", ">"];

struct Checker {
    issues: Vec<Issue>,
}

impl Checker {
    fn required(&mut self, present: bool, path: &str) {
        if !present {
            self.issues.push(Issue::error(
                "required",
                path,
                format!("{path} is required"),
            ));
        }
    }

    fn code(&mut self, value: &str, allowed: &[&str], path: &str) {
        if value.is_empty() {
            self.required(false, path);
        } else if !allowed.contains(&value) {
            self.issues.push(Issue::error(
                "code-invalid",
                path,
                format!("{value:?} is not a valid {path} code"),
            ));
        }
    }

    fn optional_code(&mut self, value: Option<&str>, allowed: &[&str], path: &str) {
        if let Some(value) = value {
            self.code(value, allowed, path);
        }
    }

    fn concept(&mut self, concept: &CodeableConcept, path: &str) {
        let has_content = concept.text.as_deref().is_some_and(|t| !t.is_empty())
            || concept
                .coding
                .iter()
                .any(|c| c.code.as_deref().is_some_and(|code| !code.is_empty()));
        if !has_content {
            self.issues.push(Issue::error(
                "required",
                path,
                format!("{path} needs a coding with a code, or text"),
            ));
        }
    }

    fn quantity(&mut self, quantity: &Quantity, path: &str) {
        self.optional_code(
            quantity.comparator.as_deref(),
            COMPARATOR,
            &format!("{path}.comparator"),
        );
    }

    fn notes(&mut self, notes: &[Annotation], path: &str) {
        for (i, note) in notes.iter().enumerate() {
            self.required(!note.text.is_empty(), &format!("{path}[{i}].text"));
        }
    }

    fn patient(&mut self, patient: &Patient, path: &str) {
        self.optional_code(patient.gender.as_deref(), GENDER, &format!("{path}.gender"));
    }

    fn specimen(&mut self, specimen: &Specimen, path: &str) {
        self.optional_code(
            specimen.status.as_deref(),
            SPECIMEN_STATUS,
            &format!("{path}.status"),
        );
        self.notes(&specimen.note, &format!("{path}.note"));
    }

    fn service_request(&mut self, request: &ServiceRequest, path: &str) {
        self.code(&request.status, REQUEST_STATUS, &format!("{path}.status"));
        self.code(&request.intent, REQUEST_INTENT, &format!("{path}.intent"));
        self.optional_code(
            request.priority.as_deref(),
            PRIORITY,
            &format!("{path}.priority"),
        );
        self.required(request.subject.is_some(), &format!("{path}.subject"));
        if let Some(code) = &request.code {
            self.concept(code, &format!("{path}.code"));
        }
        self.notes(&request.note, &format!("{path}.note"));
    }

    fn observation(&mut self, observation: &Observation, path: &str) {
        self.code(
            &observation.status,
            OBSERVATION_STATUS,
            &format!("{path}.status"),
        );
        self.concept(&observation.code, &format!("{path}.code"));
        let values = [
            observation.value_quantity.is_some(),
            observation.value_codeable_concept.is_some(),
            observation.value_string.is_some(),
            observation.value_boolean.is_some(),
            observation.value_range.is_some(),
            observation.value_ratio.is_some(),
            observation.value_date_time.is_some(),
        ];
        if values.iter().filter(|present| **present).count() > 1 {
            self.issues.push(Issue::error(
                "structure",
                format!("{path}.value[x]"),
                "an observation has at most one value[x]",
            ));
        }
        if observation.effective_date_time.is_some() && observation.effective_period.is_some() {
            self.issues.push(Issue::error(
                "structure",
                format!("{path}.effective[x]"),
                "an observation has at most one effective[x]",
            ));
        }
        if let Some(quantity) = &observation.value_quantity {
            self.quantity(quantity, &format!("{path}.valueQuantity"));
        }
        self.notes(&observation.note, &format!("{path}.note"));
    }

    fn report(&mut self, report: &DiagnosticReport, path: &str) {
        self.code(&report.status, REPORT_STATUS, &format!("{path}.status"));
        self.concept(&report.code, &format!("{path}.code"));
    }

    fn outcome(&mut self, outcome: &OperationOutcome, path: &str) {
        self.required(!outcome.issue.is_empty(), &format!("{path}.issue"));
    }

    fn bundle(&mut self, bundle: &Bundle, path: &str) {
        self.code(&bundle.kind, BUNDLE_TYPE, &format!("{path}.type"));
        let needs_request = matches!(bundle.kind.as_str(), "transaction" | "batch");
        let needs_response = matches!(
            bundle.kind.as_str(),
            "transaction-response" | "batch-response"
        );
        for (i, entry) in bundle.entry.iter().enumerate() {
            let entry_path = format!("{path}.entry[{i}]");
            if needs_request {
                match &entry.request {
                    Some(request) => {
                        self.code(
                            &request.method,
                            HTTP_METHOD,
                            &format!("{entry_path}.request.method"),
                        );
                        self.required(
                            !request.url.is_empty(),
                            &format!("{entry_path}.request.url"),
                        );
                    }
                    None => self.required(false, &format!("{entry_path}.request")),
                }
            }
            if needs_response {
                self.required(
                    entry
                        .response
                        .as_ref()
                        .is_some_and(|r| !r.status.is_empty()),
                    &format!("{entry_path}.response.status"),
                );
            }
            if let Some(resource) = &entry.resource {
                self.resource(resource, &format!("{entry_path}.resource"));
            }
        }
    }

    fn resource(&mut self, resource: &Resource, path: &str) {
        match resource {
            Resource::Patient(r) => self.patient(r, path),
            Resource::Specimen(r) => self.specimen(r, path),
            Resource::ServiceRequest(r) => self.service_request(r, path),
            Resource::Observation(r) => self.observation(r, path),
            Resource::DiagnosticReport(r) => self.report(r, path),
            Resource::Bundle(r) => self.bundle(r, path),
            Resource::OperationOutcome(r) => self.outcome(r, path),
            Resource::Device(_) | Resource::Other(_) => {}
        }
    }
}

/// Validates a resource and, for bundles, every entry. Returns the issues
/// found; an empty list means the resource passed.
pub fn validate(resource: &Resource) -> Vec<Issue> {
    let mut checker = Checker { issues: Vec::new() };
    checker.resource(resource, resource.resource_type());
    checker.issues
}

/// The issues of [`validate`] as an OperationOutcome, or `None` when the
/// resource passed.
pub fn validate_to_outcome(resource: &Resource) -> Option<OperationOutcome> {
    let issues = validate(resource);
    (!issues.is_empty()).then(|| OperationOutcome {
        issue: issues,
        ..OperationOutcome::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_missing_and_invalid_elements() {
        let resource = Resource::from_json(
            br#"{"resourceType":"Bundle","type":"transaction","entry":[
                {"resource":{"resourceType":"Observation","status":"done","code":{}}},
                {"resource":{"resourceType":"ServiceRequest","status":"active","intent":"order"},"request":{"method":"POST","url":"ServiceRequest"}}
            ]}"#,
        )
        .unwrap();
        let issues = validate(&resource);
        let paths: Vec<_> = issues.iter().map(|i| i.expression[0].as_str()).collect();
        assert_eq!(
            paths,
            [
                "Bundle.entry[0].request",
                "Bundle.entry[0].resource.status",
                "Bundle.entry[0].resource.code",
                "Bundle.entry[1].resource.subject"
            ]
        );
        assert!(issues.iter().all(Issue::is_error));
    }

    #[test]
    fn accepts_valid_resources() {
        let resource = Resource::from_json(
            br#"{"resourceType":"Observation","status":"final","code":{"text":"Glucose"},"valueQuantity":{"value":5.4,"comparator":"<"}}"#,
        )
        .unwrap();
        assert!(validate(&resource).is_empty());
        assert!(validate_to_outcome(&resource).is_none());
    }
}
