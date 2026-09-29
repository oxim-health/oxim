//! Code systems: CDA identifies them by OID, the normalized model by URI.

/// LOINC.
pub const LOINC_OID: &str = "2.16.840.1.113883.6.1";
/// SNOMED CT.
pub const SNOMED_OID: &str = "2.16.840.1.113883.6.96";
/// UCUM.
pub const UCUM_OID: &str = "2.16.840.1.113883.6.8";
/// HL7 v3 ObservationInterpretation.
pub const INTERPRETATION_OID: &str = "2.16.840.1.113883.5.83";
/// HL7 v2 table 0078 (abnormal flags).
pub const V2_ABNORMAL_FLAGS_OID: &str = "2.16.840.1.113883.12.78";
/// HL7 v3 AdministrativeGender.
pub const GENDER_OID: &str = "2.16.840.1.113883.5.1";
/// HL7 v3 Confidentiality.
pub const CONFIDENTIALITY_OID: &str = "2.16.840.1.113883.5.25";

const KNOWN: [(&str, &str); 5] = [
    (LOINC_OID, "http://loinc.org"),
    (SNOMED_OID, "http://snomed.info/sct"),
    (UCUM_OID, "http://unitsofmeasure.org"),
    (
        INTERPRETATION_OID,
        "http://terminology.hl7.org/CodeSystem/v3-ObservationInterpretation",
    ),
    (
        V2_ABNORMAL_FLAGS_OID,
        "http://terminology.hl7.org/CodeSystem/v2-0078",
    ),
];

/// Whether `text` is an OID (`1.2.840…`).
pub fn is_oid(text: &str) -> bool {
    !text.is_empty()
        && text
            .split('.')
            .all(|arc| !arc.is_empty() && arc.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `text` is a UUID in its 8-4-4-4-12 hexadecimal form.
pub fn is_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(part, len)| part.len() == len && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The model system URI for a CDA code system OID: a well-known URI, or
/// `urn:oid:` plus the OID.
pub fn system_from_oid(oid: &str) -> String {
    KNOWN
        .iter()
        .find(|(known, _)| *known == oid)
        .map_or_else(|| format!("urn:oid:{oid}"), |(_, uri)| (*uri).to_owned())
}

/// The CDA code system OID for a model system URI, if it has one.
pub fn oid_from_system(system: &str) -> Option<String> {
    if let Some((oid, _)) = KNOWN.iter().find(|(_, uri)| *uri == system) {
        return Some((*oid).to_owned());
    }
    let oid = system.strip_prefix("urn:oid:").unwrap_or(system);
    is_oid(oid).then(|| oid.to_owned())
}

/// The instance identifier root for an identifier system: an OID (with or
/// without `urn:oid:`) or a UUID (with or without `urn:uuid:`).
pub fn root_from_system(system: &str) -> Option<String> {
    if let Some(oid) = oid_from_system(system) {
        return Some(oid);
    }
    let uuid = system.strip_prefix("urn:uuid:").unwrap_or(system);
    is_uuid(uuid).then(|| uuid.to_ascii_lowercase())
}

/// The identifier system for an instance identifier root.
pub fn system_from_root(root: &str) -> String {
    if is_uuid(root) {
        format!("urn:uuid:{}", root.to_ascii_lowercase())
    } else {
        format!("urn:oid:{root}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_systems_both_ways() {
        assert_eq!(system_from_oid(LOINC_OID), "http://loinc.org");
        assert_eq!(system_from_oid("1.2.3"), "urn:oid:1.2.3");
        assert_eq!(
            oid_from_system("http://loinc.org").as_deref(),
            Some(LOINC_OID)
        );
        assert_eq!(oid_from_system("urn:oid:1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(oid_from_system("urn:oxim:astm:local"), None);
        assert!(is_oid("2.16.840.1.113883.6.1") && !is_oid("2..1") && !is_oid("a.b"));
        assert!(is_uuid("0192f1e8-7c00-7000-8000-000000000001"));
        assert_eq!(
            root_from_system("urn:uuid:0192F1E8-7C00-7000-8000-000000000001").as_deref(),
            Some("0192f1e8-7c00-7000-8000-000000000001")
        );
        assert_eq!(system_from_root("1.2.3"), "urn:oid:1.2.3");
    }
}
