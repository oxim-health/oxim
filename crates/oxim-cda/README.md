# oxim-cda

HL7 CDA R2 documents for [OXIM](../../README.md): header and section extraction, laboratory results mapped to the normalized model, and laboratory report generation.

CDA documents are XML, so OXIM parses them with the lossless XML document of `oxim-formats` (`data_type: cda`): a document that passes through unmodified keeps every byte, and channel paths such as `/ClinicalDocument/title` work as for any XML.

```rust
use oxim_cda::{header, lab_results, sections};

let header = header(&document)?;        // id, code, title, time, patients, authors, custodian
let sections = sections(&document)?;    // codes, titles, narrative text, entries
let results = lab_results(&document)?;  // ClinicalContent::Results
```

| Function | Purpose |
|---|---|
| `header` | document identifier, type code, title, effective time, confidentiality, language, patients (`recordTarget`), authors (people and devices), custodian |
| `sections` | section codes, titles, narrative text (whitespace collapsed), entries and nested sections |
| `lab_results` | result organizers and observations as normalized results; see the `results` module for the field-by-field rules |
| `encode_lab_report` | a minimal CDA R2 laboratory report (LOINC `11502-2`) from normalized results |
| `register` | the `cda` normalizer and the `cda-lab-report` encoder for channels |

```yaml
destinations:
  - id: repository
    type: file
    encoder:
      type: cda-lab-report
      title: Laboratory Report
      custodian_name: Example Laboratory
      custodian_id_root: 2.999.1          # an OID or UUID
      patient_id_root: 2.999.2            # for patient identifiers without an OID system
      utc_offset: 180
```

- **Results:** organizers become result groups with their specimen; loose observations of a section form one group. Quantities keep their exact decimal text, one-sided intervals become comparators (`<0.5`), ratios, coded values, text, booleans, times and base64 attachments are supported. Only event-mood observations are results. Laboratory reports (`11502-2`) are read in full; other documents only in their laboratory sections.
- **Values carried, never interpreted** ([ADR 0011](../../docs/adr/0011-no-clinical-interpretation.md)): interpretation codes pass through unchanged and units are never converted.
- **Code systems:** CDA OIDs map to the URIs of the normalized model (LOINC, SNOMED CT, UCUM, HL7 interpretation codes), others to `urn:oid:`; the generator maps them back.
- **Report generation:** `typeId`, a document identifier derived from the OXIM message identifier, patient, device author, custodian, one results section with a narrative table and one `organizer` per result group. Implementation guide templates (for example C-CDA or IHE XD-LAB) are not asserted.

Namespace URIs are not resolved; elements are matched by local name. CDA documents with non-XML bodies have no sections.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
