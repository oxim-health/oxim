# oxim-mapping

Mappings between protocol messages and the normalized clinical model of [OXIM](../../README.md).

| Protocol | Normalizes | Encodes |
|---|---|---|
| ASTM E1394 (CLSI LIS02) | results, QC, orders, host queries | orders and host query answers (`astm-orders`, `astm-query-response`) |
| HL7 v2 | `ORU`/`OUL` results, `ORM`/`OML` orders, `QRY`/`QBP` queries | `ORU^R01` results and QC (`hl7v2-oru-r01`), `OML^O21` orders (`hl7v2-oml-o21`) |
| POCT1-A | `OBS` observations and QC, `DST`/`EVS` device events | — |
| JSON | — | the normalized content (`clinical-json`) |

- **Documented rules:** every module lists its field-by-field mapping as tables.
- **Values carried, never interpreted:** numbers stay exact decimal text (`5.40` stays `5.40`), times keep their recorded precision, abnormal flags pass through as codes, units are never converted.
- **Deterministic output:** MSH-10 is the OXIM message identifier and MSH-7 the receive time; nothing reads the clock.
- **Engine integration:** `oxim_mapping::register(&mut registry)` adds the normalizers and encoders, so a channel with `normalize: true` on an ASTM source and an `hl7v2-oru-r01` encoder turns analyzer results into ORU messages for the LIS.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
