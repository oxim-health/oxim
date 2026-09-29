# oxim-anonymize

Removes protected health information from HL7 v2, ASTM, POCT1-A, JSON and XML messages and from `.oximcap` captures, so recordings of real devices can be shared, attached to [OXIM](../../README.md) device profiles and committed to repositories.

```text
oxim-anonymize chem-1.oximcap                           # writes chem-1.anon.oximcap
oxim-anonymize *.hl7 --out anonymized/ --free-text      # several files into a directory
oxim-anonymize run.astm --key 00112233445566778899aabbccddeeff --date-shift-days -120
oxim-anonymize export.json --config rules.yaml --report report.json
```

| Input | What changes |
|---|---|
| HL7 v2 | PID, PD1, NK1, PV1, PV2, IN1, GT1 and MRG identifiers, names, addresses, telephone numbers and dates; order, specimen, container and query identifiers; every date in MSH, EVN, ORC, OBR, OBX, SPM, TQ1 and QRD |
| ASTM E1394 | P records (identifiers, name, birth date, address, telephone, physician); specimen and query identifiers; every date in H, P, O, R and Q records |
| POCT1-A | patient and operator identifiers and names, specimen and order identifiers, birth dates and every `*_dttm` time |
| JSON, XML | the paths listed in a configuration file |
| `.oximcap` | the messages inside MLLP frames, LIS01 frames (with recomputed checksums), unframed ASTM and POCT1-A, keeping the conversation replayable |

- **Pseudonyms, not blanks:** identifiers become different identifiers of the same shape (digits stay digits, letters stay letters), names become invented names. The same value gets the same pseudonym within a run, so a patient's messages and a tube's query and results still belong together. Pseudonyms come from HMAC-SHA-256 under a key that is random per run unless `--key` (or `OXIM_ANONYMIZE_KEY`) supplies one.
- **Dates move together:** every date shifts by the same number of days (random, 30 to 730 days back, unless `--date-shift-days` is given), so intervals such as age at collection survive. Times of day, time zones and recorded precision are kept.
- **Removed:** addresses, telephone numbers, national identifiers, mothers' maiden names, aliases and birthplaces.
- **Free text:** notes and comments may name people; `--free-text` replaces HL7 NTE and text OBX values, ASTM comments and POCT1-A notes with `REDACTED`.
- **Results are untouched:** values, units, reference ranges, flags and test codes stay as they are.
- **Report:** counts of changes per location (`HL7 PID-5: 2 pseudonymized`) and warnings, never the values themselves, the key or the date offset.

Captures keep their shape: a message's new bytes go into the record where the original message ended, so answers still follow what they answer and `oxim-sim replay` and `oxim profile test` work on the anonymized capture. Frames that cannot be decoded and incomplete MLLP frames are dropped with a warning; streams of an unknown protocol are kept unchanged with a warning.

## Configuration

```yaml
free_text: false        # as --free-text
specimens: true         # false keeps specimen, order and container identifiers (--keep-specimen-ids)
json:
  - {path: patient.identifier[*].value, action: pseudonymize}
  - {path: patient.name, action: name}
  - {path: patient.birthDate, action: shift-date}
  - {path: patient.address, action: remove}
  - {path: notes[*], action: redact}
xml:
  - {path: /order/patient/@id, action: pseudonymize}
  - {path: "/order/sample[*]/@collected", action: shift-date}
```

JSON paths use dotted member names, `[n]`, `[*]` (every element) and `*` (every member). XML paths follow `oxim-formats` (`/a/b[2]/@attr`) with `[*]` for every repeated element. An action on an object or array applies to every value inside it.

Anonymization is a tool, not a guarantee. Z-segments, unknown record types and free text without `--free-text` are kept: review the output before sharing it.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
