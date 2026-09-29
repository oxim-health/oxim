//! The document header and the section structure.

use oxim_formats::{XmlDocument, XmlElement};
use oxim_model::{
    AdministrativeSex, ClinicalDateTime, CodeableConcept, Coding, Device, HumanName, Identifier,
    Patient,
};

use crate::codes::{system_from_oid, system_from_root};
use crate::error::{CdaError, CdaResult};

/// The header of a CDA document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CdaHeader {
    /// `id`: the document identifier.
    pub id: Option<Identifier>,
    /// `code`: the document type, for example LOINC `11502-2`.
    pub code: Option<CodeableConcept>,
    /// `title`.
    pub title: Option<String>,
    /// `effectiveTime`: when the document was created.
    pub effective_time: Option<ClinicalDateTime>,
    /// `confidentialityCode/@code`, for example `N`.
    pub confidentiality: Option<String>,
    /// `languageCode/@code`.
    pub language: Option<String>,
    /// `recordTarget/patientRole`: the patients the document is about.
    pub patients: Vec<Patient>,
    /// `author`: people and devices that wrote the document.
    pub authors: Vec<Author>,
    /// `custodian`: the organization that keeps the document.
    pub custodian: Option<Organization>,
}

/// One `author` of the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Author {
    /// `time`.
    pub time: Option<ClinicalDateTime>,
    /// `assignedAuthor/id`.
    pub identifiers: Vec<Identifier>,
    /// `assignedAuthor/assignedPerson/name`.
    pub person: Option<HumanName>,
    /// `assignedAuthor/assignedAuthoringDevice`.
    pub device: Option<Device>,
    /// `assignedAuthor/representedOrganization/name`.
    pub organization: Option<String>,
}

/// An organization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Organization {
    /// `id`.
    pub identifiers: Vec<Identifier>,
    /// `name`.
    pub name: Option<String>,
}

/// One section of the structured body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Section {
    /// `code`.
    pub code: Option<CodeableConcept>,
    /// `title`.
    pub title: Option<String>,
    /// The narrative block (`text`) as plain text, whitespace collapsed.
    pub text: String,
    /// The entries.
    pub entries: Vec<Entry>,
    /// Nested sections (`component/section`).
    pub sections: Vec<Section>,
}

/// One `entry` of a section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// `@typeCode`, for example `DRIV`.
    pub type_code: Option<String>,
    /// The clinical statement's element name, for example `organizer`.
    pub statement: String,
    /// The clinical statement's `@classCode`.
    pub class_code: Option<String>,
    /// The clinical statement's `code`.
    pub code: Option<CodeableConcept>,
}

/// The root element, checked to be `ClinicalDocument`.
pub(crate) fn root(document: &XmlDocument) -> CdaResult<XmlElement<'_>> {
    let root = document.root();
    if root.local_name() == "ClinicalDocument" {
        Ok(root)
    } else {
        Err(CdaError::NotCda(root.name()))
    }
}

fn nonempty(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// Narrative block elements, whose text is kept apart from its neighbors.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "paragraph"
            | "br"
            | "list"
            | "item"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "caption"
    )
}

/// Text with runs of whitespace collapsed to one space.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A coded element (`CD`, `CE`, `CV`, `CS`) as a concept: the code and its
/// translations as codings, `originalText` (else the first display name)
/// as text.
pub(crate) fn concept(element: XmlElement<'_>) -> Option<CodeableConcept> {
    let mut codings = Vec::new();
    for coded in std::iter::once(element).chain(element.children_named("translation")) {
        if let Some(code) = nonempty(coded.attribute("code")) {
            let system = nonempty(coded.attribute("codeSystem"))
                .map(|oid| system_from_oid(&oid))
                .or_else(|| nonempty(coded.attribute("codeSystemName")));
            codings.push(Coding {
                system,
                code,
                display: nonempty(coded.attribute("displayName")),
            });
        }
    }
    let text = element
        .child("originalText")
        .map(|t| collapse(&t.text_content()))
        .filter(|t| !t.is_empty());
    (!codings.is_empty() || text.is_some()).then_some(CodeableConcept { codings, text })
}

/// An instance identifier (`II`). The extension is the value and the root
/// the system; a root alone is the value.
pub(crate) fn identifier(element: XmlElement<'_>) -> Option<Identifier> {
    let root = nonempty(element.attribute("root"));
    let extension = nonempty(element.attribute("extension"));
    let assigner = nonempty(element.attribute("assigningAuthorityName"));
    let (value, system) = match (extension, root) {
        (Some(extension), root) => (extension, root.map(|r| system_from_root(&r))),
        (None, Some(root)) => (root, None),
        (None, None) => return None,
    };
    Some(Identifier {
        system,
        value,
        kind: None,
        assigner,
    })
}

/// A time stamp (`TS`) from `@value`, or from `low/@value` of an interval.
pub(crate) fn time(element: XmlElement<'_>) -> Option<ClinicalDateTime> {
    nonempty(element.attribute("value"))
        .or_else(|| {
            element
                .child("low")
                .and_then(|low| nonempty(low.attribute("value")))
        })
        .and_then(|value| ClinicalDateTime::parse_hl7(&value).ok())
}

/// A person name (`PN`).
pub(crate) fn name(element: XmlElement<'_>) -> Option<HumanName> {
    let parts = |part: &str| -> Vec<String> {
        element
            .children_named(part)
            .map(|p| collapse(&p.text_content()))
            .filter(|p| !p.is_empty())
            .collect()
    };
    let name = HumanName {
        family: parts("family").into_iter().next(),
        given: parts("given"),
        prefix: parts("prefix").into_iter().next(),
        suffix: parts("suffix").into_iter().next(),
    };
    if name == HumanName::default() {
        // A name given as plain text.
        let text = collapse(&element.text_content());
        return (!text.is_empty()).then(|| HumanName {
            family: Some(text),
            ..HumanName::default()
        });
    }
    Some(name)
}

fn patient(role: XmlElement<'_>) -> Patient {
    let mut patient = Patient {
        identifiers: role.children_named("id").filter_map(identifier).collect(),
        ..Patient::default()
    };
    if let Some(person) = role.child("patient") {
        patient.name = person.child("name").and_then(name);
        patient.sex = person
            .child("administrativeGenderCode")
            .and_then(|g| g.attribute("code"))
            .and_then(|code| match code.as_str() {
                "UN" => Some(AdministrativeSex::Unknown),
                other => AdministrativeSex::from_hl7_code(other),
            });
        patient.birth_date = person.child("birthTime").and_then(time);
    }
    patient
}

fn author(element: XmlElement<'_>) -> Author {
    let assigned = element.child("assignedAuthor");
    let device = assigned
        .and_then(|a| a.child("assignedAuthoringDevice"))
        .map(|device| Device {
            model: device
                .child("manufacturerModelName")
                .map(|m| collapse(&m.text_content()))
                .filter(|m| !m.is_empty()),
            name: device
                .child("softwareName")
                .map(|s| collapse(&s.text_content()))
                .filter(|s| !s.is_empty()),
            identifiers: assigned
                .map(|a| a.children_named("id").filter_map(identifier).collect())
                .unwrap_or_default(),
            ..Device::default()
        });
    Author {
        time: element.child("time").and_then(time),
        identifiers: assigned
            .map(|a| a.children_named("id").filter_map(identifier).collect())
            .unwrap_or_default(),
        person: assigned
            .and_then(|a| a.child("assignedPerson"))
            .and_then(|p| p.child("name"))
            .and_then(name),
        device,
        organization: assigned
            .and_then(|a| a.child("representedOrganization"))
            .and_then(|o| o.child("name"))
            .map(|n| collapse(&n.text_content()))
            .filter(|n| !n.is_empty()),
    }
}

/// Reads the header of a CDA document.
pub fn header(document: &XmlDocument) -> CdaResult<CdaHeader> {
    let root = root(document)?;
    let custodian = root
        .child("custodian")
        .and_then(|c| c.child("assignedCustodian"))
        .and_then(|c| c.child("representedCustodianOrganization"))
        .map(|organization| Organization {
            identifiers: organization
                .children_named("id")
                .filter_map(identifier)
                .collect(),
            name: organization
                .child("name")
                .map(|n| collapse(&n.text_content()))
                .filter(|n| !n.is_empty()),
        });
    Ok(CdaHeader {
        id: root.child("id").and_then(identifier),
        code: root.child("code").and_then(concept),
        title: root
            .child("title")
            .map(|t| collapse(&t.text_content()))
            .filter(|t| !t.is_empty()),
        effective_time: root.child("effectiveTime").and_then(time),
        confidentiality: root
            .child("confidentialityCode")
            .and_then(|c| nonempty(c.attribute("code"))),
        language: root
            .child("languageCode")
            .and_then(|c| nonempty(c.attribute("code"))),
        patients: root
            .children_named("recordTarget")
            .filter_map(|target| target.child("patientRole"))
            .map(patient)
            .collect(),
        authors: root.children_named("author").map(author).collect(),
        custodian,
    })
}

/// The top-level sections of the structured body.
pub(crate) fn body_sections<'a>(root: XmlElement<'a>) -> impl Iterator<Item = XmlElement<'a>> + 'a {
    root.child("component")
        .and_then(|c| c.child("structuredBody"))
        .into_iter()
        .flat_map(|body| body.children_named("component"))
        .filter_map(|component| component.child("section"))
}

/// The clinical statement inside an entry, entry relationship or
/// component: its first child element other than `sequenceNumber` and
/// `seperatableInd`.
pub(crate) fn statement(holder: XmlElement<'_>) -> Option<XmlElement<'_>> {
    holder
        .children()
        .find(|child| !child.is("sequenceNumber") && !child.is("seperatableInd"))
}

fn section(element: XmlElement<'_>) -> Section {
    Section {
        code: element.child("code").and_then(concept),
        title: element
            .child("title")
            .map(|t| collapse(&t.text_content()))
            .filter(|t| !t.is_empty()),
        text: element
            .child("text")
            .map(|t| collapse(&t.text_content_with(is_block)))
            .unwrap_or_default(),
        entries: element
            .children_named("entry")
            .filter_map(|entry| {
                let statement = statement(entry)?;
                Some(Entry {
                    type_code: nonempty(entry.attribute("typeCode")),
                    statement: statement.local_name(),
                    class_code: nonempty(statement.attribute("classCode")),
                    code: statement.child("code").and_then(concept),
                })
            })
            .collect(),
        sections: element
            .children_named("component")
            .filter_map(|c| c.child("section"))
            .map(section)
            .collect(),
    }
}

/// Reads the sections of the structured body, with their nested sections.
/// A document with a non-XML body has none.
pub fn sections(document: &XmlDocument) -> CdaResult<Vec<Section>> {
    Ok(body_sections(root(document)?).map(section).collect())
}
