//! ASTM E1381 and E1394 (CLSI LIS01-A2 and LIS02-A2) for laboratory
//! instruments.
//!
//! - [`frame`]: LIS01 frames with checksums, and splitting messages into
//!   frames.
//! - [`session`]: a sans-IO LIS01 link session that sends and receives
//!   messages with the ENQ/ACK/EOT handshake, retransmissions, timeouts,
//!   contention and receiver interrupts.
//! - [`raw`]: splitting unframed record streams (ASTM over TCP without
//!   LIS01) into messages.
//! - [`Message`]: a lossless LIS02 message model with path access
//!   (`R-4`, `R[2]-3.4`), editing, escape sequences and the patient → order
//!   → result hierarchy.
//!
//! The crate performs no I/O and never reads the clock.
//!
//! ```
//! use oxim_astm::Message;
//!
//! let input = b"H|\\^&|||ANALYZER|||||LIS||P|1\r\
//! P|1||PID001||Doe^Jane\r\
//! O|1|SMP001||^^^GLU\r\
//! R|1|^^^GLU|5.4|mmol/L|3.9^6.1|N||F\r\
//! L|1|N\r";
//!
//! let message = Message::parse(input)?;
//! assert_eq!(message.to_bytes(), input);
//! // ASTM numbering: the record type is field 1, so R-4 is the value.
//! assert_eq!(message.get("R-3.4").unwrap(), "GLU");
//! assert_eq!(message.get("R-4").unwrap(), "5.4");
//!
//! let patients = message.patients();
//! assert_eq!(patients[0].orders[0].results.len(), 1);
//! # Ok::<(), oxim_astm::ParseError>(())
//! ```

mod delimiters;
mod error;
mod escape;
pub mod frame;
mod hierarchy;
mod message;
mod path;
pub mod raw;
mod record;
pub mod session;
mod value;

pub use delimiters::Delimiters;
pub use error::{FrameError, ParseError, PathError, SendError};
pub use escape::{escape, unescape};
pub use hierarchy::{OrderGroup, PatientGroup, ResultGroup};
pub use message::{Message, ParseOptions, RecordTerminators};
pub use path::{FieldPath, MAX_INDEX, Path};
pub use record::{LineEnding, RecordMut, RecordRef};
pub use value::{Level, Parts, Value};

/// Re-exported so callers can name encodings without depending on
/// `encoding_rs` directly.
pub use encoding_rs::Encoding;
