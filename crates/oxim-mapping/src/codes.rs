//! Code systems.
//!
//! HL7 v2 names code systems with short table values (`LN`, `SCT`,
//! `UCUM`); the normalized model uses the FHIR URIs where one exists. Other
//! names are kept verbatim in both directions, so a local system such as
//! `99LAB` survives a round trip.
//!
//! | HL7 v2 | Normalized model |
//! |---|---|
//! | `LN` | `http://loinc.org` |
//! | `SCT`, `SNM` | `http://snomed.info/sct` |
//! | `UCUM` | `http://unitsofmeasure.org` |
//! | `HL70078` | `http://terminology.hl7.org/CodeSystem/v2-0078` |
//! | `L` (local code, written for [`ASTM_LOCAL`] and [`ASTM_UNIVERSAL`]) | `L` |
//! | anything else | unchanged |
//!
//! ASTM codes use URNs defined by OXIM:
//!
//! | Source | System |
//! |---|---|
//! | Manufacturer's local code (component 4 of a universal test ID) | [`ASTM_LOCAL`] |
//! | Universal test ID (component 1), unless component 3 names LOINC | [`ASTM_UNIVERSAL`] |
//! | The complete universal test ID field, kept for lossless re-encoding | [`ASTM_TEST_ID`] |

use oxim_model::{CodeableConcept, Coding};

/// LOINC.
pub const LOINC: &str = "http://loinc.org";
/// SNOMED CT.
pub const SNOMED: &str = "http://snomed.info/sct";
/// UCUM units.
pub const UCUM: &str = "http://unitsofmeasure.org";
/// HL7 table 0078, abnormal flags.
pub const HL7_ABNORMAL_FLAGS: &str = "http://terminology.hl7.org/CodeSystem/v2-0078";
/// Manufacturer-specific test codes from ASTM universal test IDs.
pub const ASTM_LOCAL: &str = "urn:oxim:astm:local";
/// Universal test IDs from ASTM records that do not name a known system.
pub const ASTM_UNIVERSAL: &str = "urn:oxim:astm:universal";
/// The complete ASTM universal test ID field, including instrument-specific
/// components such as dilution or replicate.
pub const ASTM_TEST_ID: &str = "urn:oxim:astm:test-id";

/// The model system for an HL7 v2 coding system name.
pub fn system_from_hl7(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(
        match name {
            "LN" => LOINC,
            "SCT" | "SNM" => SNOMED,
            "UCUM" => UCUM,
            "HL70078" => HL7_ABNORMAL_FLAGS,
            other => other,
        }
        .to_owned(),
    )
}

/// The HL7 v2 coding system name for a model system.
pub fn system_to_hl7(system: &str) -> &str {
    match system {
        LOINC => "LN",
        SNOMED => "SCT",
        UCUM => "UCUM",
        HL7_ABNORMAL_FLAGS => "HL70078",
        // HL7 table 0396: "L" is a local general code.
        ASTM_LOCAL | ASTM_UNIVERSAL => "L",
        other => other,
    }
}

/// A coding from non-empty parts.
pub(crate) fn coding(code: &str, display: Option<&str>, system: Option<String>) -> Option<Coding> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    Some(Coding {
        system,
        code: code.to_owned(),
        display: display
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_owned),
    })
}

/// The coding to write into a single code field: the one in `preferred`
/// system if present, otherwise the primary one.
pub(crate) fn pick<'a>(concept: &'a CodeableConcept, preferred: &[&str]) -> Option<&'a Coding> {
    preferred
        .iter()
        .find_map(|system| {
            concept
                .codings
                .iter()
                .find(|c| c.system.as_deref() == Some(system))
        })
        .or_else(|| {
            concept
                .codings
                .iter()
                .find(|c| c.system.as_deref() != Some(ASTM_TEST_ID))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_systems_both_ways() {
        for name in ["LN", "SCT", "UCUM", "HL70078", "99LAB", "L"] {
            let system = system_from_hl7(name).unwrap();
            assert_eq!(system_to_hl7(&system), name);
        }
        assert_eq!(system_from_hl7("SNM").as_deref(), Some(SNOMED));
        assert_eq!(system_from_hl7(" "), None);
    }
}
