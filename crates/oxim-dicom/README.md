# oxim-dicom

DICOM for [OXIM](../../README.md): storage, query/retrieve, modality worklist, modality performed procedure steps, storage commitment, DICOMweb, attribute filters and editing, and PS3.15 de-identification. Register them with `oxim_dicom::register_with(&mut registry, &DicomEnvironment::new(data_dir))` (or `register` for an in-memory environment); channel files then refer to them by type name.

| Type | Kind | What it does |
|---|---|---|
| `dicom-scp` | source | Storage SCP (C-STORE, C-ECHO), optionally storage commitment |
| `dicom-qr-scp` | source | C-FIND SCP (Patient Root, Study Root) over the instance index |
| `dicom-mwl-scp` | source | Modality Worklist SCP, optionally with MPPS |
| `dicom-mpps-scp` | source | Modality Performed Procedure Step SCP |
| `dicom-retrieved` | source | Stores the instances `dicom-get` and `dicomweb-wado` retrieve |
| `dicom-scu` | destination | Storage SCU (C-STORE), optionally with storage commitment |
| `dicom-find` | destination | C-FIND SCU |
| `dicom-move` | destination | C-MOVE SCU |
| `dicom-get` | destination | C-GET SCU |
| `dicomweb-stow` | destination | STOW-RS |
| `dicomweb-qido` | destination | QIDO-RS |
| `dicomweb-wado` | destination | WADO-RS |
| `dicom-tag` | filter | Keeps messages by the value of an attribute |
| `dicom-set` | transformer | Sets and removes attributes |
| `dicom-deidentify` | transformer | PS3.15 Basic Profile de-identification |
| `dicom-index` | transformer | Records objects in the instance index |
| `worklist-from-orders` | transformer | Schedules worklist items from normalized orders |
| `hl7v2-orm-status` | encoder | A performed procedure step as an HL7 v2 status update |

The components of one engine share a `DicomEnvironment`: the instance index (`dicom-index.db`) and the worklist with the performed procedure steps (`worklist.db`) in the data directory, opened on first use, the inboxes of `dicom-retrieved` sources and pending storage commitment requests. Both databases are derived data.

A channel with `data_type: dicom` carries each DICOM instance as a Part 10 object: preamble, `DICM`, file meta information and the data set in the transfer syntax the meta information names. The networking uses [dicom-rs](https://github.com/Enet4/dicom-rs) (`dicom-ul`), and objects are decoded with `dicom-object` after a structural check of every declared length and of the sequence depth, so malformed input is rejected instead of exhausting memory.

```yaml
id: ct-to-pacs
source:
  type: dicom-scp
  data_type: dicom
  settings:
    listen: 0.0.0.0:11112
    ae_title: OXIM
    calling_ae_titles: [CT1, CT2]
filters:
  - type: dicom-tag
    tag: Modality
    in: [CT, SR]
destinations:
  - id: pacs
    type: dicom-scu
    settings:
      target: pacs.example:104
      called_ae_title: PACS
  - id: research
    type: dicomweb-stow
    transformers:
      - type: dicom-deidentify
        secret_env: OXIM_DEID_SECRET
        date_shift_max_days: 365
    settings:
      url: https://research.example/dicom-web
      bearer_token_env: RESEARCH_TOKEN
```

Durations are written like `250ms`, `5s`, `2m`. Attributes are named by keyword (`PatientID`), tag (`(0010,0020)`, `0010,0020` or `00100020`) or nested path (`RequestAttributesSequence[0].AccessionNumber`).

## `dicom-scp`: Storage SCP (source)

| Setting | Default | Meaning |
|---|---|---|
| `listen` | required | Address to listen on, for example `0.0.0.0:104` |
| `ae_title` | `OXIM` | AE title of the SCP |
| `calling_ae_titles` | any | Calling AE titles allowed to associate |
| `check_called_ae_title` | `true` | Reject associations addressed to another AE title |
| `sop_classes` | common storage classes | Accepted SOP classes as UIDs or keywords (`CTImageStorage`); `["*"]` accepts any |
| `transfer_syntaxes` | any readable | Accepted transfer syntax UIDs in order of preference |
| `max_pdu_length` | `16384` | Largest PDU accepted |
| `max_object_size` | 512 MiB | Largest data set accepted |
| `max_associations` | `32` | Simultaneous associations; more are closed |
| `timeout` | `60s` | Time to wait for the next PDU of an association |
| `index` | `false` | Record every stored object in the instance index |
| `storage_commitment` | `false` | Offer the Storage Commitment Push Model (implies `index`) |
| `tls` | none | TLS listener settings, see [TLS](#tls) |

The SCP answers C-ECHO and C-STORE. Associations from unlisted calling AE titles, or addressed to another called AE title, are rejected (A-ASSOCIATE-RJ). Verification is always accepted; the default storage classes cover radiography, CT, MR, NM, PET, ultrasound, secondary capture, visible light, ophthalmology, RT, registration, segmentation, presentation states, structured reports, encapsulated documents and waveforms.

Each C-STORE is answered **Success (`0000`) only after the object is stored durably**. If it cannot be stored the answer is Refused: Out of Resources (`A700`); an object over `max_object_size` gets `A700` as well, and a request without SOP class or instance UID or without a data set gets Processing Failure (`0110`). Other DIMSE requests get Unrecognized Operation (`0211`).

The data set is stored exactly as received, in the negotiated transfer syntax, behind new file meta information (source AE title = `ae_title`, sending = calling AE title, receiving = called AE title). The message records the peer address and this metadata:

| Key | Value |
|---|---|
| `dicom.calling_ae` | Calling AE title |
| `dicom.called_ae` | Called AE title |
| `dicom.sop_class_uid` | Affected SOP Class UID |
| `dicom.sop_instance_uid` | Affected SOP Instance UID |
| `dicom.transfer_syntax` | Negotiated transfer syntax |
| `dicom.study_instance_uid`, `dicom.series_instance_uid`, `dicom.modality` | From the data set, when present |

## `dicom-scu`: Storage SCU (destination)

| Setting | Default | Meaning |
|---|---|---|
| `target` | required | Receiver address, `host:port` |
| `called_ae_title` | `ANY-SCP` | AE title of the receiver |
| `calling_ae_title` | `OXIM` | AE title OXIM presents |
| `connect_timeout` | `10s` | Time to establish the TCP connection |
| `timeout` | `60s` | Limit for each read and write, including the wait for the response |
| `max_pdu_length` | `16384` | Largest PDU OXIM accepts from the receiver |
| `transcode` | `true` | Convert native data sets when the receiver does not accept their transfer syntax |
| `storage_commitment` | none | `{timeout: 60s, association_wait: 5s}`: request storage commitment after each C-STORE |
| `tls` | none | TLS sender settings, see [TLS](#tls) |

Each delivery opens one association, sends one C-STORE and releases the association. The SCU proposes the object's SOP class in the object's transfer syntax; for native (uncompressed) objects it also proposes Explicit and Implicit VR Little Endian and converts the data set if only those are accepted. **Compressed pixel data is never decompressed**: a receiver that does not accept the object's compressed transfer syntax fails the delivery.

| Outcome | Delivery |
|---|---|
| Success `0000`, warnings `0001`, `0107`, `0116`, `Bxxx` | Delivered; the status is stored as the response (JSON) |
| `A7xx` (out of resources), `0110` (processing failure), `0213` (resource limitation) | Retried |
| `A9xx`, `Cxxx` and other failures | Failed |
| Connection refused, timeouts, rejected or aborted associations | Retried |
| No accepted presentation context, or not a Part 10 object | Failed |

A rejected association (for example an unknown calling AE title) is retried because the receiver's configuration can change; with strict ordering it holds the queue until then.

## Storage commitment

With `storage_commitment` on a `dicom-scu` destination, each stored instance is followed by an N-ACTION (Storage Commitment Push Model, transaction UID generated per request), and the delivery succeeds only once the receiver commits to the instance. The SCU waits `association_wait` for the N-EVENT-REPORT on the same association, then releases it and waits, up to `timeout`, for the report on an association the archive opens: that association must reach an OXIM `dicom-scp` with `storage_commitment: true` in the same process (configure its AE title and port on the archive). A failure reason of `0110`, `0213` or `A7xx`, or no report in time, is retried; other reasons (`0112` no such instance, `0119` class/instance conflict, ...) fail the delivery.

With `storage_commitment: true` on a `dicom-scp` source, OXIM itself commits: it answers N-ACTION with Success and sends the N-EVENT-REPORT on the same association right away, committing to every instance it has stored durably (found in the instance index with the same SOP class) and failing the others with `0112` (or `0119`). Reports are only sent on the requesting association.

## `dicom-qr-scp`, `dicom-mwl-scp`, `dicom-mpps-scp` (sources)

| Setting | Default | Meaning |
|---|---|---|
| `listen` | required | Address to listen on |
| `ae_title` | `OXIM` | AE title of the SCP |
| `calling_ae_titles` | any | Calling AE titles allowed to associate |
| `check_called_ae_title` | `true` | Reject associations addressed to another AE title |
| `max_pdu_length` | `16384` | Largest PDU accepted |
| `max_associations` | `32` | Simultaneous associations |
| `timeout` | `60s` | Time to wait for the next PDU |
| `max_results` | `1000` | Most matches returned for one query |
| `record_queries` | `false` | Store every C-FIND identifier as a message (audit trail) |
| `mpps` | `false` | `dicom-mwl-scp` only: also accept MPPS on the worklist AE |
| `tls` | none | TLS listener settings |

`dicom-qr-scp` answers C-FIND of the Patient Root and Study Root information models from the instance index, at the PATIENT (Patient Root only), STUDY, SERIES and IMAGE levels, with `NumberOf...Related...` counts and `ModalitiesInStudy`. A Query/Retrieve Level that the model does not allow is answered with `A900`.

`dicom-mwl-scp` answers Modality Worklist C-FIND from the worklist: items that are `SCHEDULED` or `IN PROGRESS` and not expired.

Matching follows PS3.4 C.2.2.2: single values, universal matching (empty keys return the value), wildcards `*` and `?` (not for UIDs; person names regardless of letter case), date and time ranges (`20240101-20240131`, `-20240131`, `1000-1200`), lists of UIDs and sequence matching (for example on the Scheduled Procedure Step Sequence). Responses carry exactly the requested keys and `SpecificCharacterSet` `ISO_IR 192`. A C-CANCEL arriving after the matches were sent is ignored.

`dicom-mpps-scp` (and `dicom-mwl-scp` with `mpps: true`) accepts N-CREATE and N-SET of Modality Performed Procedure Steps. N-CREATE must be `IN PROGRESS` (an instance UID is assigned when the modality leaves it out); N-SET merges the modified attributes into the step while it is `IN PROGRESS`, and may complete or discontinue it. Each request is answered with Success only after the **merged** step is stored as a message (a Part 10 object of the MPPS SOP class, metadata `dicom.command`, `dicom.mpps_status`, `dicom.accession_number`). Duplicate N-CREATE is `0111`, N-SET of an unknown step `0112`, of a finished step `0110`. The worklist items the step references (Accession Number and Scheduled Procedure Step ID of the Scheduled Step Attributes Sequence) take its status.

## `worklist-from-orders` (transformer)

| Setting | Default | Meaning |
|---|---|---|
| `modality` | required | Modality of the scheduled steps, for example `CT` |
| `modalities` | none | Modality by test code, overriding `modality` |
| `station_ae_title` | none | Scheduled Station AE Title |
| `stations` | none | Scheduled Station AE Title by modality |
| `station_name` | none | Scheduled Station Name |
| `expire_after` | `7d` | How long after its scheduled start an item is offered |
| `utc_offset` | `0` | UTC offset in minutes of scheduled times taken from the receive time |

The step works on normalized `Orders` (for example from HL7 `ORM`/`OML` with `normalize: true`) and passes other content unchanged:

| Order | Worklist item |
|---|---|
| Filler order number, else placer order number | Accession Number (16 characters) |
| Test position | Requested Procedure ID and Scheduled Procedure Step ID: the accession number, with `.n` for the n-th test of a multi-test order |
| Test code, display and system | Requested Procedure Code Sequence and Scheduled Protocol Code Sequence; text as Requested Procedure Description |
| Requested date/time, else the receive time | Scheduled Procedure Step Start Date and Time |
| Priority | Requested Procedure Priority (`STAT`, `HIGH`, `ROUTINE`) |
| Patient identifier, name, birth date, sex | Patient ID (with issuer), Patient's Name, Birth Date, Sex |
| Placer and filler order numbers | Placer/Filler Order Number / Imaging Service Request |
| Accession number and procedure ID | Study Instance UID (`2.25.` UID derived from both, the same every time) |

New and added orders create or update items, a replacement replaces the items of its accession number and a cancellation discontinues them.

## `hl7v2-orm-status` (encoder)

Turns a stored performed procedure step into an `ORM^O01` status update for the order placer (IHE Radiology Scheduled Workflow): ORC-1 `SC`, ORC-5 `IP` (in progress), `CM` (completed) or `DC` (discontinued), and one ORC/OBR pair per referenced requested procedure.

| MPPS | HL7 v2 |
|---|---|
| Patient ID (issuer), Patient's Name, Birth Date, Sex | PID-3 (CX.4), PID-5, PID-7, PID-8 |
| Placer / Filler Order Number / Imaging Service Request | ORC-2 and OBR-2 / ORC-3 and OBR-3 (Accession Number when no filler number) |
| Procedure Code Sequence | OBR-4, else the Requested Procedure Description as text |
| Start / end date and time | OBR-7 / OBR-8; ORC-9 is the end for completed and discontinued steps, else the start |
| Accession Number, Requested Procedure ID, Scheduled Procedure Step ID | OBR-18, OBR-19, OBR-20 |
| Modality | OBR-24 |

Settings: `sending_application` (`OXIM`), `sending_facility`, `receiving_application`, `receiving_facility`, `version` (`2.5.1`), `processing_id` (`P`), `utc_offset` (minutes, for MSH-7). The encoder handles only performed procedure steps, so as a reply encoder it skips other messages.

## Identifiers

`dicom-find`, `dicom-move`, `dicom-get`, `dicomweb-qido` and `dicomweb-wado` take the query identifier from the delivery's payload, for example built by a `map` or `script` step from an order:

- a JSON map of attribute names (keywords or tags) to values: `{"QueryRetrieveLevel": "STUDY", "PatientID": "SYN-0001", "StudyDate": "20240101-20240131", "StudyInstanceUID": null}`; `null` or `""` asks for the attribute as a return key, arrays are multiple values, objects (or arrays of objects) are sequence items;
- a DICOM JSON object (PS3.18 F.2);
- a DICOM data set as a Part 10 object.

The `keys` setting adds keys the payload leaves out.

## `dicom-find`, `dicom-move`, `dicom-get` (destinations)

| Setting | Default | Meaning |
|---|---|---|
| `target` | required | Address of the SCP |
| `called_ae_title` | `ANY-SCP` | AE title of the SCP |
| `calling_ae_title` | `OXIM` | AE title OXIM presents |
| `connect_timeout` | `10s` | Time to establish the TCP connection |
| `timeout` | `60s` (find), `10m` (move, get) | Limit for each read and write, including the wait between responses |
| `max_pdu_length` | `16384` | Largest PDU accepted |
| `model` | `study_root` | `study_root`, `patient_root`, or `worklist` (find only) |
| `keys` | none | Keys added to every identifier |
| `max_results` | `1000` | find: most matches kept; the query is cancelled with C-CANCEL after that many |
| `move_destination` | `calling_ae_title` | move: the AE the SCP sends the instances to, usually an OXIM `dicom-scp` |
| `into` | required for get | get: the inbox of the `dicom-retrieved` source that stores the instances |
| `sop_classes` | common storage classes | get: storage SOP classes accepted (at most 120) |
| `max_object_size` | 512 MiB | get: largest instance accepted |
| `retry_partial` | `false` | move, get: retry the whole retrieval when some instances failed (`B000`) |
| `tls` | none | TLS sender settings |

`dicom-find` stores `{"status", "count", "truncated", "matches": [DICOM JSON...]}` as the response, so a channel with `source.response.mode: destination` can relay query results. `dicom-move` and `dicom-get` store the final status and the sub-operation counts. `dicom-get` proposes the storage classes with SCP role selection and the native and common compressed transfer syntaxes; every instance it receives is stored through the inbox before its C-STORE sub-operation is answered with Success (`A700` when it could not be stored).

| Final status | Delivery |
|---|---|
| Success, warnings (`B000` counts failures) | Delivered (retried with `retry_partial` when instances failed) |
| `A7xx`, `0110`, `0213`, `FE00` (cancel) | Retried |
| `A801` (move destination unknown), `A900`, `Cxxx` and other failures | Failed |

## `dicom-retrieved` (source)

| Setting | Default | Meaning |
|---|---|---|
| `inbox` | required | Name that `dicom-get` and `dicomweb-wado` destinations deliver to |
| `queue` | `16` | Instances that may wait to be stored |

The source stores each retrieved instance as a message of its channel and confirms the storage to the destination, which only then acknowledges the instance. While no source holds the inbox, retrievals fail and are retried.

## `dicomweb-qido`, `dicomweb-wado` (destinations)

Both accept `url` (base URL of the DICOMweb service), `bearer_token` or `bearer_token_env`, `headers`, `timeout`, `ca_file` and `max_response_size`, like `dicomweb-stow`.

`dicomweb-qido` searches `{url}/{level}` where `level` is `studies` (default), `series` or `instances`: keys with values become matching parameters, keys without values become `includefield`, and `params` are added to every search (for example `limit`). The response body (`application/dicom+json`, `[]` for `204`) is stored.

`dicomweb-wado` retrieves `{url}/studies/{study}[/series/{series}[/instances/{instance}]]` from the identifier's UIDs, accepting `multipart/related; type="application/dicom"`, and stores every part through the inbox named by `into`. The response stores how many instances were retrieved and their message identifiers. `timeout` defaults to `10m` and `max_response_size` to 1 GiB, since the instances are held in memory.

## `dicom-index` (transformer)

Records each DICOM message in the instance index (patient, study, series and instance attributes, and the message identifier), for `dicom-qr-scp` and storage commitment. It has no settings.

## TLS

Every accepting component takes a `tls` block with `cert_file`, `key_file`, optional `client_ca_file` (mutual TLS), `require_client_cert` and `handshake_timeout`; every requesting component takes `ca_file`, `system_roots`, optional `cert_file`/`key_file` and `server_name`. They are the settings of the MLLP and TCP connectors (see `oxim-connectors`), with rustls and the ring provider; certificate files are read when the channel is deployed.

## `dicomweb-stow`: STOW-RS (destination)

| Setting | Default | Meaning |
|---|---|---|
| `url` | required | Base URL of the DICOMweb service; objects are posted to `{url}/studies` |
| `bearer_token` | none | Sent as `Authorization: Bearer ...` |
| `bearer_token_env` | none | Environment variable holding the bearer token |
| `headers` | none | Extra request headers |
| `timeout` | `120s` | Limit for the whole request |
| `ca_file` | none | PEM file with extra trusted certificate authorities |
| `max_response_size` | 16 MiB | Largest accepted response body |

Each delivery is one `multipart/related; type="application/dicom"` request with the Part 10 object, accepting `application/dicom+json`. HTTPS uses rustls with the operating system's trusted certificates plus `ca_file`. `200` delivers the message and the response body is stored; `202` delivers it too unless the Failed SOP Sequence lists the instance. A failed instance fails the delivery, unless its failure reason may pass later (`A7xx`, `0110`, `0213`), which is retried. `408`, `429`, `3xx`, `5xx` and transport errors are retried; other `4xx` responses fail the delivery.

## `dicom-tag` (filter)

| Setting | Meaning |
|---|---|
| `tag` | The attribute |
| `equals` | Keep when the value, or one of its values, equals this text |
| `in` | Keep when the value, or one of its values, is in this list |
| `exists` | Keep when the attribute is present (`true`) or absent (`false`) |
| `negate` | Invert the result |

Exactly one of `equals`, `in` and `exists` is required. Text is compared without padding.

## `dicom-set` (transformer)

| Setting | Meaning |
|---|---|
| `set` | Map of attribute to value: text (multiple values separated by `\`), a number, or `{value, vr}` for attributes outside the standard dictionary |
| `remove` | Attributes to remove |
| `remove_private` | Remove every private attribute, in nested sequences too |

Missing sequences and items on a nested path are created. The object is encoded again in its transfer syntax, with sequences and items of undefined length; the file meta information follows the SOP Class and Instance UIDs of the data set.

## `dicom-deidentify` (transformer)

| Setting | Default | Meaning |
|---|---|---|
| `secret` / `secret_env` | required | Secret (at least 16 characters), or the environment variable holding it |
| `date_shift_days` | none | Shift every date by this many days |
| `date_shift_max_days` | none | Shift each patient's dates by a per-patient offset within ± this many days |
| `pseudonymize_patient` | `true` | Replace Patient Name and ID with a pseudonym; `false` empties them |
| `pseudonym_prefix` | `ANON` | Prefix of the pseudonym, followed by `-` and 16 hexadecimal digits |
| `keep` | none | Attributes kept unchanged, for example `[PatientSex, PatientAge]` |
| `remove_private` | `true` | Remove private attributes |
| `allow_burned_in_annotation` | `false` | Accept objects whose Burned In Annotation is `YES` |

The step applies the Basic Application Level Confidentiality Profile of PS3.15 annex E:

- Attributes of table E.1-1 are removed (X), emptied (Z) or replaced with a dummy value (D), in nested sequences too. Where the profile allows several actions the attribute is emptied if emptying is allowed and removed otherwise.
- Every UID is replaced, except standard UIDs (root `1.2.840.10008`) and attributes naming classes or syntaxes such as SOP Class UID. The replacement is `2.25.` followed by a UUID derived with HMAC-SHA-256 from the secret and the original UID, so it is the same in every object and after restarts: a study stays one study, references between objects stay valid, and without the secret the mapping cannot be reversed.
- Private attributes, curve data and overlay data and comments are removed.
- Patient Name and Patient ID become the same pseudonym for every object of a patient.
- With `date_shift_days` or `date_shift_max_days` (Retain Longitudinal Temporal Information with Modified Dates option), DA values and the date part of DT values are shifted instead of removed, and times are kept.
- Patient Identity Removed, De-identification Method, De-identification Method Code Sequence (`113100`, plus `113107` with date shifting) and Longitudinal Temporal Information Modified are set, and the AE titles are dropped from the file meta information.

Pixel data is not changed: objects with burned-in annotation are rejected unless `allow_burned_in_annotation` is set, and text inside structured reports is removed with the Content Sequence rather than cleaned.

## Limits

- Objects are held in memory in full while they are received, stored, filtered, transformed and sent, and conversion needs a second copy; size memory accordingly and keep `max_object_size` realistic.
- Deflated Explicit VR Little Endian and unknown private transfer syntaxes are not supported: the SCP does not accept them and the steps reject them.
- There is no DICOM normalizer, because the clinical model has no imaging resources; route with `dicom-tag` and the `dicom.*` metadata.
- `dicom-qr-scp` answers C-FIND only; it does not serve C-MOVE or C-GET of the objects it indexed (the objects are stored as messages, not in an archive).
- Relational queries and extended negotiation (fuzzy semantic matching, timezone adjustment, enhanced multi-frame conversion) are not supported.
- Storage commitment reports are sent by OXIM only on the requesting association, and reports for OXIM's own requests arrive on a separate association only through a `dicom-scp` with `storage_commitment: true` in the same process.
- Worklist items and performed procedure steps live in one process's `worklist.db`; several OXIM instances do not share them.
