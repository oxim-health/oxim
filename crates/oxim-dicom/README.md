# oxim-dicom

DICOM routing for [OXIM](../../README.md): a Storage SCP source, Storage SCU and DICOMweb STOW-RS destinations, an attribute filter, an attribute editor and PS3.15 de-identification. Register them with `oxim_dicom::register(&mut registry)`; channel files then refer to them by type name.

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

Each delivery opens one association, sends one C-STORE and releases the association. The SCU proposes the object's SOP class in the object's transfer syntax; for native (uncompressed) objects it also proposes Explicit and Implicit VR Little Endian and converts the data set if only those are accepted. **Compressed pixel data is never decompressed**: a receiver that does not accept the object's compressed transfer syntax fails the delivery.

| Outcome | Delivery |
|---|---|
| Success `0000`, warnings `0001`, `0107`, `0116`, `Bxxx` | Delivered; the status is stored as the response (JSON) |
| `A7xx` (out of resources), `0110` (processing failure), `0213` (resource limitation) | Retried |
| `A9xx`, `Cxxx` and other failures | Failed |
| Connection refused, timeouts, rejected or aborted associations | Retried |
| No accepted presentation context, or not a Part 10 object | Failed |

A rejected association (for example an unknown calling AE title) is retried because the receiver's configuration can change; with strict ordering it holds the queue until then.

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
- DICOM TLS, query/retrieve (C-FIND, C-MOVE, C-GET), Modality Worklist, MPPS, Storage Commitment and QIDO-RS/WADO-RS are not provided by this crate.
