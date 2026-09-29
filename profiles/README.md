# Device profiles

A device profile tells OXIM how to work with one device model: how it connects, which protocol variant it speaks, the channel that talks to it, default code tables, known quirks, a setup guide, and recorded sessions that prove the profile works. The format is described in [`oxim-devices`](../crates/oxim-devices/README.md).

The profiles here are generic examples, one per protocol family, with synthetic fixtures recorded from `oxim-sim`:

| Profile | Protocol | Verification |
|---|---|---|
| [`generic-astm-analyzer`](generic-astm-analyzer/profile.yaml) | ASTM E1381/E1394 (LIS01/LIS02) over TCP, results and host queries | simulated |
| [`generic-ihe-law-analyzer`](generic-ihe-law-analyzer/profile.yaml) | IHE LAW: HL7 v2.5.1 OUL^R22 and QBP^Q11 over MLLP | simulated |
| [`generic-poct1a-device`](generic-poct1a-device/profile.yaml) | CLSI POCT1-A over TCP | simulated |

```text
oxim profile validate generic-astm-analyzer/profile.yaml
oxim profile test generic-astm-analyzer/profile.yaml
```

## Writing a profile for a real device

1. Copy the closest generic profile into a new directory named after the model.
2. Fill in vendor, model, firmware range and the dialect from the vendor's interface specification; set the level to `documented` with the specification as evidence.
3. Record the device with `oxim-capture` (the proxy sits between the device and OXIM or the existing LIS).
4. Remove protected health information with `oxim-anonymize` and check the result.
5. Add the capture as a fixture, run `oxim profile test --bless` once, review the expected output, then run `oxim profile test`.
6. Raise the level to `lab-verified` or `field-verified` only with dated evidence of where and when the device was tested.

Real-device profiles are collected in the separate `oxim-health/device-profiles` repository. Fixtures must never contain real patient data.
