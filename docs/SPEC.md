# OXIM Product and Engineering Specification

> **Status:** pre-1.0, under active development. **Not for production use.**
> No release has been published. The first tagged release will be the complete 1.0 described in this document.

OXIM stands for **Open eXchange for Interoperable Medicine**.

## Contents

1. [Product definition](#1-product-definition)
2. [System overview](#2-system-overview)
3. [Standards and data formats](#3-standards-and-data-formats)
4. [Connectors](#4-connectors)
5. [Engine capabilities](#5-engine-capabilities)
6. [Transformation and mapping](#6-transformation-and-mapping)
7. [Device connectivity toolkit](#7-device-connectivity-toolkit)
8. [Operations and observability](#8-operations-and-observability)
9. [Administration](#9-administration)
10. [Security and privacy](#10-security-and-privacy)
11. [Deployment, scale and high availability](#11-deployment-scale-and-high-availability)
12. [Extensibility](#12-extensibility)
13. [Migration from Mirth Connect](#13-migration-from-mirth-connect)
14. [Architecture](#14-architecture)
15. [Quality engineering](#15-quality-engineering)
16. [Performance targets](#16-performance-targets)
17. [Project and community](#17-project-and-community)
18. [Build sequence to 1.0](#18-build-sequence-to-10)
19. [1.0 acceptance criteria](#19-10-acceptance-criteria)
20. [Decisions](#20-decisions)

---

## 1. Product definition

OXIM is an open-source clinical integration engine that runs on-premise, including on air-gapped networks. It connects devices (laboratory analyzers, point-of-care devices, imaging modalities) to systems (LIS, HIS/EHR, RIS, PACS, billing, registries). It receives, validates, transforms, routes and delivers messages with guaranteed delivery, and makes every step visible to operators.

### Users

- Hospital and laboratory IT teams.
- LIS and HIS/EHR vendors, who can embed OXIM in their products.
- Integration consultants.
- Device manufacturers, who can use OXIM and its simulators as a test tool.

### Boundaries

These are permanent product boundaries, not deferred features.

- **OXIM is not a LIS, EHR or PACS.** It is not a clinical system of record, it does not archive images and it does not provide a viewer.
- **OXIM does not interpret clinical data.** It moves and reformats data. It ships no rules that compute, verify or flag clinical results. See [ADR 0011](adr/0011-no-clinical-interpretation.md).
- **No cloud dependency and no telemetry.** An optional update check exists and is disabled by default.

### Fundamentals

| Item | Value |
|---|---|
| License | MIT OR Apache-2.0 |
| Language | Rust. SQLite and QuickJS are embedded C components. |
| Project language | English for all project content. The UI is translation-ready. |
| Runtime model | Local-first, single binary, works without internet access |

---

## 2. System overview

```
 Devices and systems                          OXIM node(s)                              Target systems
 ───────────────────        ┌─────────────────────────────────────────────────┐       ──────────────
 Analyzers (ASTM/HL7)  ───▶ │ Sources ─▶ Filters ─▶ Transformers ─▶ Router     │ ───▶  LIS / HIS / EHR
 POCT (POCT1-A)        ───▶ │    │                                   │         │ ───▶  RIS / PACS
 Modalities (DICOM)    ───▶ │    ▼                                   ▼         │ ───▶  FHIR servers
 HIS/EHR (HL7/FHIR)    ───▶ │ Durable store ◀── per-destination queues ──▶     │ ───▶  Databases / queues / files
 Files, databases, APIs───▶ │ Order cache · Device registry · Code tables      │ ───▶  Other OXIM channels
                            │ Web UI · REST API · CLI · Alerts · Metrics       │
                            └─────────────────────────────────────────────────┘
```

To devices, OXIM behaves like the LIS or host system they expect. To the LIS, OXIM appears as a single source that delivers one normalized, standards-based output, regardless of how many device dialects sit behind it.

---

## 3. Standards and data formats

| Area | Coverage |
|---|---|
| **HL7 v2.x (2.1–2.9)** | Lossless ER7 parse and serialize; HL7 v2 XML encoding; batch files (FHS/BHS/BTS/FTS); original and enhanced acknowledgment modes; conformance profile validation; escape sequences; Z-segments |
| **MLLP** | MLLP release 1 and release 2 (transport-level commit acknowledgment); TLS and mutual TLS |
| **HL7 FHIR R4 / R4B / R5** | Resource models generated from the official StructureDefinitions; JSON and XML; REST client (CRUD, search, transactions, Subscriptions); REST facade that accepts FHIR resources as a source; structural and profile validation; external terminology server client |
| **HL7 v2 ↔ FHIR** | Mappings based on the HL7 v2-to-FHIR Implementation Guide |
| **HL7 CDA R2 / v3 XML** | Parse, generate, section extraction |
| **ASTM E1381 / E1394 (CLSI LIS01 / LIS02)** | Complete sender and receiver state machines: timers, retries, contention rules, frame splitting and reassembly, checksums; all four delimiters declared in the header record; raw mode without LIS01 framing |
| **POCT1-A (CLSI POCT01)** | Device messaging layer and observation reporting: HEL, DST, OBS, EOT, ACK conversations; directives (operator lists, lockouts); QC results and device events |
| **IHE profiles** | LAW (Laboratory Analytical Workflow), LTW (Laboratory Testing Workflow), PIX/PDQ (HL7 v2 and FHIR PIXm/PDQm), XDS.b and MHD document sharing |
| **DICOM** | DIMSE services (C-ECHO, C-STORE, C-FIND, C-MOVE, C-GET); Modality Worklist SCP and SCU; MPPS; Storage Commitment; DICOMweb client (QIDO-RS, WADO-RS, STOW-RS); tag morphing; de-identification per PS3.15. Existing dicom-rs crates are reused where they fit. |
| **Other data types** | X12, NCPDP, JSON (JSONPath), XML (XPath, XSD validation), delimited/CSV, fixed-width, raw bytes, attachments (PDF, Base64-encoded data in OBX ED fields) |
| **Terminology** | Versioned code tables; import from CSV and FHIR ConceptMap/ValueSet. LOINC and SNOMED CT are never bundled because of their licenses; users import them under their own license. |

---

## 4. Connectors

Every connector can act as a source, a destination, or both, where the transport allows it.

| Family | Connectors |
|---|---|
| Stream | MLLP; raw TCP with custom framing (start/end bytes, length prefix); TLS and mutual TLS |
| Serial | RS-232 and RS-485; USB-to-serial hot-plug; serial-over-IP (RFC 2217 and raw terminal servers) |
| Web | HTTP(S) listener and sender; REST; webhooks; SOAP (WS-Security, MTOM); OAuth2 client credentials |
| Files | Local filesystem; SMB/CIFS shares; FTP, FTPS, SFTP; S3-compatible object storage. Atomic read and write conventions. |
| Databases | PostgreSQL, MySQL/MariaDB, Microsoft SQL Server, Oracle, SQLite, ODBC. Polling reader and writer. |
| Messaging | Kafka, AMQP (RabbitMQ), MQTT, NATS |
| Email | SMTP sender; IMAP reader |
| Imaging | DICOM SCP and SCU; DICOMweb |
| Internal | Channel-to-channel routing; scheduled/timer source; custom connectors as WebAssembly plugins |

---

## 5. Engine capabilities

### 5.1 Channels and routing

- A channel is a pipeline: source → filters → transformers → one or more destinations → response transformers → post-processor.
  - Destinations run sequentially, in parallel, or conditionally.
  - Channels support deploy and undeploy scripts; global scripts apply across channels.
- Content-based routing.
- Splitting: a batch message is split into individual messages.
- Aggregation: several messages are combined into one, with a timeout. Example: multiple ASTM results aggregated into a single ORU message.
- Enrichment: lookups against databases or HTTP services during processing.
- Throttling and message priorities.

### 5.2 Delivery guarantees

- **Durable before acknowledge.** A source receives its acknowledgment only after the message has been durably written to the store. Group commit keeps throughput high. See [ADR 0004](adr/0004-durable-before-acknowledge.md).
- **Per-destination FIFO queues.** Each channel/destination pair has its own ordered queue. A failing destination affects only its own queue. See [ADR 0005](adr/0005-per-destination-queues.md).
- Failed deliveries are retried with exponential backoff and eventually move to an error (dead-letter) state.
- Single-message and bulk reprocessing from the UI, CLI or API.
- Idempotency keys and duplicate detection on message control IDs.

### 5.3 Laboratory workflows (bidirectional)

- **Results.** Device message → normalized model → code mapping → one standardized output. The default output is HL7 v2 ORU^R01; FHIR, JSON and database rows are available as alternatives.
- **Orders.** Orders from the LIS are held in an order cache and routed to the analyzer(s) that perform each test, using test-to-device routing tables.
  - Load balancing across analyzers that perform the same test.
  - Rerun and reflex test requests.
- **Host queries.** When a device scans a specimen barcode and asks for its orders, OXIM answers in real time: first from the order cache, otherwise by passing the query to the LIS within a configured timeout. A standards-conformant "no orders" reply is returned when nothing is found. Host queries bypass the delivery queues because devices expect an answer within seconds.
- **QC and device events.** QC results, calibration events and instrument events are routed to their own destinations.

### 5.4 Hospital workflows

- ADT feed fan-out to subscribing systems.
- Order routing to departments (ORM/OML).
- Result delivery to HIS/EHR (ORU).
- Scheduling (SIU), documents (MDM/CDA) and billing (DFT) flows.

### 5.5 Imaging workflows

- Modality Worklist generated from HL7 or FHIR orders.
- MPPS notifications converted into HL7 status updates.
- DICOM store-and-forward routing with tag morphing and de-identification in transit.
- DICOMweb client support.
- No image archive and no viewer (see [Boundaries](#boundaries)).

### 5.6 Message store

- Every message is stored with its raw form, its content at each processing stage, all responses and all errors.
- Full-text search across message content and metadata.
- Retention and pruning policies; compressed archiving.
- Export and import of messages.
- Privacy tooling: search, export and erase all messages for a given patient (supporting data subject requests under GDPR, KVKK and similar regulations).

---

## 6. Transformation and mapping

- **Lossless per-format trees.** Every supported format is parsed into a tree that serializes back to the identical bytes when unmodified. The raw message is always retained. See [ADR 0007](adr/0007-lossless-trees-and-fhir-aligned-model.md).
- **Normalized clinical model.** A lean subset of FHIR: Observation, ServiceRequest, Specimen, Patient, Device and DiagnosticReport. OXIM does not invent its own semantic model or wire protocol.
- **Declarative mapping language.** YAML with an expression language for field mapping, constants, formatting and conditions.
- **Code tables.** Versioned tables (for example, analyzer code `GLU` → LIS code `1520`), editable in the UI.
- **Outbound message templates.**
- **JavaScript.** Scripts run in an embedded QuickJS runtime inside a sandbox with CPU-time and memory limits and no filesystem or network access unless explicitly granted.
- **WebAssembly plugins.** Custom transformation logic in any language that compiles to WebAssembly.
- **Built-in converters.** HL7 v2 ↔ FHIR, ASTM → HL7 v2 / FHIR, POCT1-A → HL7 v2 / FHIR.
- **Patient identity cross-reference tables.** Mapping of patient identifiers between systems. This is not a Master Patient Index.

---

## 7. Device connectivity toolkit

Connecting real devices is the hardest part of clinical integration. OXIM treats it as a first-class concern.

- **Device registry.** All connected devices with status, last message time and firmware version.
- **Device profiles.** A profile describes a device model:
  - vendor, model and firmware range;
  - transport and protocol dialect settings;
  - known quirks and deviations from the standard;
  - default mappings;
  - a step-by-step setup guide (for example, which menu on the analyzer holds the host IP and port);
  - a verification level.
- **Verification levels.**

  | Level | Meaning |
  |---|---|
  | Unverified | Profile exists; no evidence yet |
  | Documented | Based on the vendor's interface specification |
  | Simulated | Passes recorded or specification-based simulated sessions |
  | Lab-verified | Verified against a real device in a test environment |
  | Field-verified | Verified in production use at a real site |

- **Community profile library** in a separate repository, `oxim-health/device-profiles`, with CI validation of every contributed profile and its fixtures.
- **Tools:**
  - `oxim-sim` — simulators for analyzers, LIS, HIS and imaging modalities; replays recorded sessions.
  - `oxim-capture` — serial port and network capture for building evidence (network capture through the companion netKit project).
  - `oxim-anonymize` — removes protected health information from captures before they are shared or committed.
  - `oxim profile test` — runs a profile against its recorded fixtures and reports conformance.
- **Silence alarms.** An alert fires when a device has not sent a message within a configured period.

---

## 8. Operations and observability

- Dashboard with per-channel statistics (received, filtered, queued, sent, errored).
- End-to-end message tracing across channels.
- Structured logs, Prometheus metrics and OpenTelemetry traces (exported to a local collector).
- Alert engine:
  - Rules: queue depth, error rate, device silence, disk space, certificate expiry.
  - Notification targets: email, webhooks, Microsoft Teams and Slack, SNMP traps (for hospital network operation centers), syslog.
- System health view, backup and restore commands, maintenance mode.

---

## 9. Administration

- **Web UI** (Svelte and TypeScript, embedded in the binary, served locally, live updates over WebSocket):
  - dashboard;
  - visual channel designer with an integrated code editor;
  - message browser (raw, tree, transformed and response views side by side; reprocessing);
  - devices, code tables and alerts;
  - users, roles and settings;
  - audit log, system health and backups;
  - accessibility conformance with WCAG 2.1 AA.
- **CLI.** Every operation available in the UI is available on the command line.
- **REST API** with a published OpenAPI specification. The UI uses the same API.
- **Configuration as code:**
  - channels are YAML files that can be versioned in git (see [ADR 0008](adr/0008-channels-as-yaml-files.md));
  - environment profiles (for example test and production) and references to secrets;
  - channel version history with diff and rollback;
  - hot deployment of individual channels;
  - import and export;
  - a community channel template repository, `oxim-health/channel-templates`.

---

## 10. Security and privacy

- **Authentication:** local accounts (Argon2 password hashing), LDAP and Active Directory, single sign-on via OIDC and SAML, multi-factor authentication (TOTP and WebAuthn).
- **Authorization:** fine-grained role-based access control, including per-channel permissions.
- **Protected health information:**
  - field masking by role;
  - break-glass access, which requires a written justification and is recorded in the audit log.
- **Audit log:** tamper-evident; entries are linked in a hash chain.
- **Encryption:**
  - TLS and mutual TLS on every network connector, with certificate management;
  - encryption at rest (AES-GCM envelope encryption);
  - secrets vault backed by a master key, the operating system key store, or an external Vault.
- **Isolation:** scripts and plugins run sandboxed; rate limiting; Content Security Policy for the UI.
- **Supply chain:** signed releases, SBOM, cargo-deny in CI, reproducible builds, a written threat model.
- **Regulatory support:** retention policies, patient-based search, export and erase, and audit logging to support GDPR, KVKK and HIPAA obligations. These are tools, not a certification claim.
- **Secure defaults:** the web UI and API bind to `localhost` by default; login is always required.

---

## 11. Deployment, scale and high availability

- **Platforms:** a single binary for Windows, Linux and macOS on x64 and ARM64. ARM64 support allows low-cost installation on small edge computers inside clinics.
- **Packages:** Windows service with an MSI installer; systemd units; deb and rpm packages; Docker/OCI images; a Kubernetes Helm chart; an offline installation bundle for air-gapped networks.
- **Upgrades:** automatic database migrations, a backup before every upgrade, and rollback support.
- **Storage:** SQLite for single-node installations; PostgreSQL for clusters. See [ADR 0006](adr/0006-storage-sqlite-and-postgresql.md).
- **Cluster mode:**
  - Coordination runs through PostgreSQL: listener ownership through leases and advisory locks; queue sharing through `SELECT … FOR UPDATE SKIP LOCKED`. No additional coordination infrastructure (such as etcd or ZooKeeper) is required.
  - Active-passive and active-active topologies, behind a virtual IP or a load balancer.
  - A shared blob store (filesystem, NFS or S3-compatible) for large attachments.
  - Failover target: under 30 seconds, without message loss.

The `deploy/` directory contains the machine-readable deployment artifacts (Dockerfile, Helm chart, systemd units, the WiX installer definition, package scripts and the offline bundle builder). Human-readable installation guides live in `docs/`.

---

## 12. Extensibility

- **Native connectors** are Rust crates compiled into the binary and selected with Cargo feature flags.
- **WebAssembly plugins** (component model) can implement connectors, data types and transformers. They are loaded at runtime and run in a sandbox.
- **Scripting API** for JavaScript transformers and filters.
- **REST API, webhooks and an event stream** for external integration.
- **Protocol libraries** are published as independent crates on crates.io.
- **Other languages:** a C ABI (`oxim-ffi`) and a Python package (PyO3) expose the protocol libraries to C, C++, Python and any language with a C FFI.

---

## 13. Migration from Mirth Connect

- **Import** of Mirth Connect channels, code templates and global scripts.
- **E4X compatibility shim.** Mirth Connect transformers use E4X. OXIM emulates the bracket-access style (`msg['PID']['PID.5']['PID.5.1']`) on top of its message trees. E4X XML literal syntax is not supported.
- **Migration report** listing every script line or feature that could not be converted.
- **Shadow mode** (optional). With the companion netKit project, OXIM passively observes live traffic next to a running Mirth Connect installation and compares its own output with Mirth Connect's output, without touching any message.
- **Message history import** from Mirth Connect (optional).

---

## 14. Architecture

### 14.1 Repository layout

```
oxim/
├─ crates/
│  ├─ protocols: oxim-hl7, oxim-mllp, oxim-astm, oxim-poct1a, oxim-fhir, oxim-cda,
│  │             oxim-dicom, oxim-x12, oxim-ncpdp, oxim-formats (JSON/XML/CSV/fixed-width)
│  ├─ oxim-model        message envelope, identifiers, normalized clinical model
│  ├─ oxim-mapping      built-in converters (HL7 v2, ASTM, POCT1-A ↔ normalized model)
│  ├─ oxim-store        storage interface + SQLite
│  ├─ oxim-store-postgres  PostgreSQL store for clusters
│  ├─ oxim-core         channel runtime, routing, queues, replies
│  ├─ oxim-lab          order cache, host queries, test routing and balancing
│  ├─ oxim-connectors   stream, file, HTTP, ASTM, POCT1-A, serial, in-process connectors, TLS
│  ├─ oxim-connectors-db, oxim-connectors-messaging, oxim-connectors-remote
│  │                    databases; MQTT/AMQP/Kafka/NATS; SFTP/FTP/S3/SMTP/IMAP/SOAP
│  ├─ oxim-transform    mapping language, code tables, templates
│  ├─ oxim-script       QuickJS sandbox + Mirth Connect compatibility
│  ├─ oxim-plugin       WebAssembly host (the SDK lives in plugin-sdk/)
│  ├─ oxim-devices      device registry + profiles + conformance runs
│  ├─ oxim-cluster      leases, failover
│  ├─ oxim-auth         users, RBAC, sessions, tokens, directory and SSO logins
│  ├─ oxim-alert        alert engine
│  ├─ oxim-server       REST API, server-sent events, embedded UI
│  ├─ oxim-mirth        Mirth Connect importer
│  ├─ oxim-ffi          C ABI
│  ├─ oxim-sim, oxim-capture, oxim-anonymize, oxim-bench   tools
│  └─ oxim              main binary + CLI
├─ bindings/python/
├─ ui/           Svelte + TypeScript
├─ plugin-sdk/
├─ profiles/     example device profiles
├─ deploy/       Docker, Helm, systemd, Windows installer, packages, offline bundle
├─ docs/         installation guides, the book, ADRs, security
└─ fuzz/

Separate repositories: oxim-health/device-profiles, oxim-health/channel-templates
```

Connector families that pull large dependency trees live in their own
crates (`oxim-connectors-db`, `-messaging`, `-remote`) instead of feature
flags of one crate, so each can be left out of a custom build; the `oxim`
binary includes all of them. Tools live in `crates/` like the libraries.

### 14.2 Runtime model

- One process on the Tokio async runtime.
- Each connector runs as its own task; each channel has its own processing pipeline; each destination has its own queue worker.
- A scheduler drives retries, pruning and polling sources.
- All persistence goes through the storage interface in `oxim-store`, with SQLite and PostgreSQL implementations.

### 14.3 Message lifecycle

```
RECEIVED ──▶ FILTERED
    │
    └──────▶ TRANSFORMED ──▶ QUEUED ──▶ SENT
                               │
                               └──────▶ ERROR ──(reprocess)──▶ QUEUED
```

- State after `TRANSFORMED` is tracked separately for every destination.
- Message identifiers are ULIDs, so they sort by creation time.
- The source acknowledgment is sent only after the `RECEIVED` state is durable.

### 14.4 Protocol layer

- Protocol crates follow the sans-IO pattern: they never open sockets or read clocks. They consume bytes and produce bytes or events; time is passed in by the caller. See [ADR 0003](adr/0003-sans-io-protocol-libraries.md).
- The same code serves active use (OXIM connectors over real sockets and serial ports) and passive use (analysis of captured traffic), and can be tested without a network.
- Protocol crates declare `#![forbid(unsafe_code)]`. `unsafe` is confined to FFI boundaries.

---

## 15. Quality engineering

- **Unit tests** for every crate.
- **Property tests:** parse → serialize → parse must reproduce identical bytes; escaping and unescaping must round-trip.
- **Protocol conformance suites** for every supported standard.
- **Fuzzing** of every parser, run continuously in CI; the project will apply to OSS-Fuzz.
- **End-to-end tests** driven by `oxim-sim`.
- **Chaos tests:** process kill (`kill -9`), full disk, fsync failure injection, network partitions, cluster failover.
- **Soak test:** seven days of continuous operation under load.
- **UI end-to-end tests** with Playwright.
- **Benchmark regression tracking** in CI.
- **Security:** cargo-deny and cargo-audit in CI, a written threat model, and an independent penetration test before 1.0.

---

## 16. Performance targets

These are **targets**, verified by the benchmark suite in CI. They are not measured results.

Reference hardware: 8 CPU cores, NVMe storage, SQLite, single node.

| Metric | Target |
|---|---|
| End-to-end HL7 v2 throughput with durable storage | at least 2,000 messages/second |
| Source acknowledgment latency at 500 messages/second | p99 ≤ 50 ms |
| Idle memory | ≤ 150 MB |
| Concurrently deployed channels | 1,000 |
| Startup time | ≤ 3 seconds |
| Restart with 1,000,000 queued messages | no message loss |

---

## 17. Project and community

- **Documentation site** (mdBook): installation, writing channels, device setup guides, Mirth Connect migration guide, security hardening guide, API reference.
- **Project files:** Architecture Decision Records in [`docs/adr/`](adr/README.md), `GOVERNANCE.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`. Significant changes go through an RFC process.
- **Versioning:** Semantic Versioning. The Rust APIs, the channel YAML schema and the plugin interface are versioned independently.
- **Release policy:** regular releases after 1.0, plus long-term support (LTS) releases.
- **Language:** all project content is in English. See [ADR 0013](adr/0013-english-only-project-content.md).

---

## 18. Build sequence to 1.0

There is no public release before 1.0. The repository is public from the first day. Internal pilot builds run at real sites from milestone M4 onward, so that real-world feedback arrives before 1.0. See [ADR 0014](adr/0014-single-complete-1-0-release.md).

| Milestone | Scope |
|---|---|
| **M0 Foundation** | Repository, CI (Windows, Linux, macOS), coding standards, ADRs, documentation skeleton |
| **M1 Protocols I** | HL7 v2, MLLP, ASTM, POCT1-A, JSON/XML/CSV/fixed-width |
| **M2 Engine core** | SQLite store, channel runtime, queues; core connectors (MLLP, ASTM over TCP/serial/RFC 2217, POCT1-A, files, HTTP); mapping and code tables; `oxim-sim` |
| **M3 Lab workflows and devices** | Order cache, host queries, test routing, QC routing; device registry, device profiles, capture and anonymization tools |
| **M4 Administration** | REST API, authentication, RBAC, audit log, complete web UI, alerts, metrics, packaging. **Internal pilot builds start here.** |
| **M5 Protocols II and connectors II** | FHIR, HL7 v2 ↔ FHIR, CDA, IHE profiles, X12, NCPDP, DICOM (DIMSE, MWL, MPPS, DICOMweb, de-identification); database, messaging, SOAP, SFTP, SMB, S3 and email connectors |
| **M6 Extensibility and migration** | QuickJS scripting, WebAssembly plugin SDK, Mirth Connect importer, netKit shadow mode, FFI and Python bindings |
| **M7 Enterprise** | PostgreSQL backend, cluster and high availability, LDAP/OIDC/SAML/MFA, encryption at rest, secrets vault, Helm chart |
| **M8 1.0 hardening** | Performance, chaos and soak testing, security review, complete documentation, accessibility, release engineering → **OXIM 1.0** |

---

## 19. 1.0 acceptance criteria

OXIM 1.0 is released only when all of the following hold:

- Every protocol and connector in this specification is implemented and passes its conformance tests.
- Zero message loss in all chaos tests.
- All performance targets are met on the reference hardware.
- Every parser completes the defined fuzzing budget without a crash; cargo-deny reports no violations; the threat model is written and reviewed.
- Every administrative operation can be performed in the UI without the CLI or YAML files; the UI conforms to WCAG 2.1 AA.
- Sample Mirth Connect channels import and produce identical output.
- MSI, deb, rpm, Docker and Helm packages are published; offline installation is verified.
- Cluster failover completes in under 30 seconds without message loss.
- Several real device profiles are field-verified.
- Documentation is complete.

---

## 20. Decisions

Each decision is recorded as an Architecture Decision Record in [`docs/adr/`](adr/README.md).

| Topic | Decision | ADR |
|---|---|---|
| Implementation language | Rust, with SQLite and QuickJS embedded as C components | [0002](adr/0002-rust-with-embedded-c-components.md) |
| Protocol libraries | Sans-IO, `forbid(unsafe_code)` | [0003](adr/0003-sans-io-protocol-libraries.md) |
| Delivery guarantee | Acknowledge only after a durable write | [0004](adr/0004-durable-before-acknowledge.md) |
| Queues | One FIFO queue per channel/destination pair | [0005](adr/0005-per-destination-queues.md) |
| Storage | SQLite for single node, PostgreSQL for clusters | [0006](adr/0006-storage-sqlite-and-postgresql.md) |
| Data model | Lossless per-format trees, FHIR-aligned normalized model, no new wire protocol | [0007](adr/0007-lossless-trees-and-fhir-aligned-model.md) |
| Configuration | Channels as YAML files | [0008](adr/0008-channels-as-yaml-files.md) |
| Web UI | Svelte and TypeScript, embedded in the binary | [0009](adr/0009-embedded-svelte-web-ui.md) |
| Extensibility | QuickJS scripting, WebAssembly plugins | [0010](adr/0010-quickjs-scripting-and-wasm-plugins.md) |
| Clinical scope | No clinical interpretation | [0011](adr/0011-no-clinical-interpretation.md) |
| License | MIT OR Apache-2.0 | [0012](adr/0012-mit-or-apache-2-licensing.md) |
| Project language | English only | [0013](adr/0013-english-only-project-content.md) |
| Release strategy | Single complete 1.0 release | [0014](adr/0014-single-complete-1-0-release.md) |

The default normalized output is HL7 v2 ORU^R01.

---

*Mirth Connect is a trademark of its owner. OXIM is an independent project and is not affiliated with, endorsed by, or sponsored by the owner of Mirth Connect. HL7, FHIR, DICOM and other standard names are trademarks of their respective owners and are used only to identify the standards OXIM implements.*
