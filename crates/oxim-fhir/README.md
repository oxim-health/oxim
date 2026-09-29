# oxim-fhir

HL7 FHIR R4 (4.0.1) for OXIM: typed JSON resources, structural
validation, mapping between FHIR and the OXIM normalized clinical model,
and summaries of transaction responses. The crate performs no I/O; bundles
reach a FHIR server through the `http` destination.

- **Resources**: Patient, Specimen, ServiceRequest, Observation,
  DiagnosticReport, Device, Bundle (with `entry.request`/`entry.response`)
  and OperationOutcome are typed. Every other resource type is kept as its
  JSON object.
- **Nothing is lost when passing through**: elements and extensions that
  are not modeled (including primitive extensions such as `_status`) are
  kept in each type's `extra` map and written back. FHIR decimals keep
  their exact text, so `5.40` stays `5.40` (serde_json's
  `arbitrary_precision`; exponents are written with a sign, `1.2e3` →
  `1.2e+3`, keeping the digits).
- **JSON only**: FHIR XML is future work.

## Public API

| Item | Purpose |
|---|---|
| `Resource::from_json`, `to_json`, `to_json_pretty` | Parse and write any resource |
| `validate`, `validate_to_outcome` | Structural checks as OperationOutcome issues |
| `encode_bundle(content, &FhirEncoding, message_id, received_at)` | Normalized model → Bundle |
| `normalize(&Resource)` | FHIR → normalized model |
| `summarize_response(bytes)` / `summarize(&Resource)` | Transaction/batch response or OperationOutcome → `ResponseSummary` |
| `entry_uuid`, `entry_url` | Deterministic `urn:uuid:` full URLs from the message id |
| `register(&mut Registry)` | Engine normalizer and encoders |

## Engine integration

`register` adds a normalizer for `data_type: fhir` and two encoders.

| Encoder | Output |
|---|---|
| `fhir-bundle` | One Bundle per message |
| `fhir-resource-json` | The single resource of the type named by `resource` (`Patient`, `Specimen`, `ServiceRequest`, `Observation`, `DiagnosticReport` or `Device`). References to other entries become logical references (`type` plus the target's first identifier). A message that maps to no resource or to several resources of that type fails: use `fhir-bundle` for it. |

| Setting | Default | Meaning |
|---|---|---|
| `bundle_type` | `transaction` | `transaction` (entries carry `request`) or `collection` (no requests). The step's `type` key names the encoder, so the bundle type has its own key. |
| `patient_identifier_system` | none | System for the first patient identifier when it has none, for example the OID of the hospital's medical record numbers |
| `specimen_identifier_system` | none | System for the first specimen identifier when it has none |
| `observation_category` | `laboratory` | Code in `http://terminology.hl7.org/CodeSystem/observation-category` |
| `utc_offset` | `0` | Minutes (-720 to 840) for times recorded without an offset |
| `pretty` | `false` | Indent the JSON |
| `resource` | required for `fhir-resource-json` | The resource type to send |

Sending results to a FHIR server: POST the transaction Bundle to the base
URL with the `http` destination.

```yaml
id: chemistry-to-fhir
source:
  type: mllp
  listen: 0.0.0.0:2575
  data_type: hl7v2
  normalize: true
destinations:
  - id: fhir
    type: http
    url: https://fhir.example.test/r4
    method: POST
    content_type: application/fhir+json
    encoder:
      type: fhir-bundle
      patient_identifier_system: urn:oid:2.16.840.1.113883.19.5
      specimen_identifier_system: urn:oid:2.16.840.1.113883.19.6
      utc_offset: 180
```

The `http` destination delivers on a `2xx` response and stores the body.
A transaction is atomic, so a `2xx` means every entry was processed;
`summarize_response` reads the stored `transaction-response` Bundle (entry
status codes and locations) or an OperationOutcome. A single Observation
can go to `[base]/Observation` with `fhir-resource-json` and
`resource: Observation`.

Receiving FHIR: a source with `data_type: fhir` and `normalize: true`
turns Observation, DiagnosticReport and ServiceRequest content (a Bundle
of any type, or a single resource) into normalized results, quality
control or orders, which any other encoder can then write, for example
`hl7v2-oru-r01`.

## Normalized model → FHIR

Each message becomes one Bundle. Entries appear in this order: Device,
then per group Patient, Specimen, ServiceRequests, Observations and the
DiagnosticReport.

| Model | FHIR |
|---|---|
| `Results.device` / `QualityControl.device` | `Device`: identifiers, `manufacturer`, `serialNumber` (also as an `urn:oxim:device-serial` identifier), `deviceName` (`user-friendly-name` from the name, `model-name` from the model), `version`. Created conditionally on its first identifier with a system; a device with no such identifier and no serial number is left out, since every message would create another Device. |
| `patient` | `Patient`: `identifier` (type coded in v2 table 0203, assigner as display), first `name`, `gender`, `birthDate` (date part), species and breed in the `patient-animal` extension. Groups with the same patient share one entry. |
| `specimen` | `Specimen`: `identifier`, `type`, `subject`, `receivedTime`, `collection.collectedDateTime`, `container.description`, `note` |
| `order` | One `ServiceRequest` per test (one without `code` when no test is listed): OXIM identifier, placer (`PLAC`) and filler (`FILL`) identifiers, `status`, `intent: order`, `priority`, `code`, `subject` (display `unidentified subject` without a patient), `authoredOn`, `specimen`, `note` |
| `observations` | One `Observation` each (see below) |
| a group's observations | One `DiagnosticReport`: OXIM identifier, `basedOn`, `status`, `category` LAB (v2 table 0074), `code` (the order's first test, or LOINC 11502-2 *Laboratory report*), `subject`, `effectiveDateTime` (first observation's), `issued` (when OXIM received the message), `specimen`, `result`, `presentedForm` (attachment results) |
| `QualityControl` results | Observations without subject, with the extra category `urn:oxim:observation-category#quality-control` and a note `Quality control material: material=…; lot=…; level=…; expires=…` |
| `Orders` | Patient, Specimen and ServiceRequests (`status` `revoked` for cancellations, `active` otherwise) |
| `Query`, `DeviceEvent` | Not mapped: the encoder fails the step |

| Observation | FHIR `Observation` |
|---|---|
| `code` | `code` (every coding, and the text) |
| `status` | `status` (same codes) |
| `effective_at`, `issued_at` | `effectiveDateTime`, `issued` |
| `operator` | `performer[0].display` |
| `device_id` | `device.display`, with a reference to the Device entry |
| `specimen_id` | `specimen.display`, with a reference to the group's Specimen |
| `method` | `method` |
| `interpretation` | `interpretation`: codes without a system, or in v2 table 0078, get the v3 ObservationInterpretation system (the same codes) |
| `reference_range` | `referenceRange[0]`: `low`/`high` carry the value's unit, `text` as is |
| `notes` | `note` |
| — | OXIM identifier, `basedOn` (the requests for its test, or all of the group's), `category` (`observation_category`), `subject` |

| Value | `value[x]` |
|---|---|
| `Quantity` | `valueQuantity`: exact decimal, `comparator`, `unit`; `system`/`code` as recorded, otherwise UCUM only when the unit looks like a UCUM code (`mmol/L`, `10*9/L`, `%`) |
| `Text` | `valueString` |
| `Coded` | `valueCodeableConcept` |
| `Range`, `Ratio` | `valueRange`, `valueRatio` |
| `Boolean` | `valueBoolean` |
| `DateTime` | `valueDateTime` |
| `Attachment` | The DiagnosticReport's `presentedForm`; the observation gets a note pointing to it |

Status of the requests and the report:

| Results | ServiceRequest | DiagnosticReport |
|---|---|---|
| all final, none amended or corrected | `completed` | `final` |
| all final, some amended or corrected | `completed` | `corrected` |
| any other status | `active` | `preliminary` |

### Identity and retries

- `fullUrl`s are `urn:uuid:` version-8 UUIDs derived from the OXIM message
  id and the entry's role (`patient-0`, `observation-0-2`, …), so
  processing a message again produces the same bundle byte for byte.
- ServiceRequests, Observations and DiagnosticReports carry an OXIM
  identifier (`urn:oxim:service-request`, `urn:oxim:observation`,
  `urn:oxim:diagnostic-report`, value `<message id>-<group>-<index>`) and
  are created with `ifNoneExist` on it, so a transaction that reached the
  server before a lost response does not create duplicates.
- Patients and specimens are created with `ifNoneExist` on their first
  identifier that has a system. An identifier without a system is never
  searched, because a bare value could match another patient's record;
  configure `patient_identifier_system` so patients are matched instead of
  created for every message.

### Times

FHIR requires seconds and a time zone whenever a time is given. Times
without an offset get `utc_offset`, times without seconds get `:00`, and
dates stay dates (`birthDate` always is one). A zero offset is written as
`Z`.

## FHIR → normalized model

| FHIR | Model |
|---|---|
| Each `DiagnosticReport` | A result group: `subject` → patient, first `specimen` → specimen, `basedOn` requests → one order (one test per request, `control` empty), `result` → observations in order, `presentedForm` → attachment values of the observations noted as attachments |
| Observations no report refers to | Result groups by subject and specimen |
| Only quality-control observations, no report | `QualityControl`, with material, lot, level and expiry read from the note |
| ServiceRequests only | `Orders`, grouped by subject, specimen and placer/filler numbers; `revoked` → cancel, anything else → new |
| First `Device` | The content's device (the `urn:oxim:device-serial` identifier is dropped) |
| `Observation.device.display`, `performer[].display`, `specimen.display` | `device_id`, `operator`, `specimen_id` |
| interpretation in the v3 system | codes without a system |
| unknown status codes | `unknown` |

References are resolved against entry `fullUrl`s and `Type/id`; absolute
URLs are matched by their last two segments (after dropping `_history`).
Unresolved references leave the patient, specimen or order empty. A
resource without any Observation, DiagnosticReport or ServiceRequest is
rejected.

What does not survive FHIR → model → FHIR: elements the model does not
carry (for example `derivedFrom`, `component`, extensions), server ids and
`meta`, v2 table 0078 as the interpretation system, and order control
codes other than new and cancel. What does: every value, decimal scale,
code, time and identifier the model carries; the tests check both
directions.

## Validation

`validate` checks required elements, the value sets of the status,
intent, priority, gender, bundle type, HTTP method and comparator codes,
one `value[x]`/`effective[x]` per observation, and transaction entry
`request`s / response entry `status`es, returning OperationOutcome issues
with FHIRPath `expression`s. It is not a profile validator: terminology
bindings, invariants and profiles are left to the server or a dedicated
validator.

## Future work

- FHIR XML.
- Host queries and device events.
- `Observation.component` for panels reported as one observation.
