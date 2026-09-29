//! Device and system simulators for testing OXIM without real hardware.
//!
//! - [`generate`]: reproducible synthetic HL7 v2 and ASTM result messages.
//! - [`mllp`]: an HL7 sender with latency statistics and an HL7 receiver
//!   that plays the LIS, with configurable acknowledgments and failures.
//! - [`astm`]: an analyzer (instrument role) and a host over ASTM LIS01.
//! - [`poct`]: a point-of-care device speaking POCT1-A.
//!
//! All data is synthetic; nothing resembles a real patient.

pub mod astm;
pub mod generate;
pub mod mllp;
pub mod poct;
