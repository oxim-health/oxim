# 0007. Lossless trees and a FHIR-aligned normalized model

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

OXIM converts between many formats (HL7 v2, ASTM, POCT1-A, FHIR, CDA, DICOM and more). Two mistakes are common in integration software:

- Parsers that silently drop or normalize content, so the output differs from the input even when nothing was changed. This breaks auditability and vendor-specific fields.
- Inventing a new internal protocol or semantic model. Such a model must be designed, documented and maintained, and no external system understands it.

Direct mapping between every pair of formats does not scale: with N input and M output formats, it needs N×M mappings.

## Decision

We will use two layers:

1. **Lossless per-format trees.** Every parser produces a tree that serializes back to the identical bytes when unmodified. Paths such as `PID-5.1` address values for reading and writing.
2. **A FHIR-aligned normalized model** for semantic conversion: a lean subset of FHIR resources (Observation, ServiceRequest, Specimen, Patient, Device, DiagnosticReport). Formats map to and from this model, which turns N×M mappings into N+M.

We will not define a new wire protocol. The raw original message is always retained alongside every processed form.

## Consequences

- Unmodified messages pass through byte-for-byte, and every transformation is auditable against the raw original.
- Semantics follow an existing, documented standard instead of a private invention.
- Simple edits (changing one field) can be made on the tree without a round trip through the normalized model.
- Some format-specific details cannot be represented in the normalized model; users who need them work on the lossless tree directly.
- Round-trip tests (parse → serialize → identical bytes) are mandatory for every parser.
