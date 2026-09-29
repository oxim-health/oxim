//! Lossless HL7 v2 messages.
//!
//! `oxim-hl7` reads, edits and writes HL7 v2.x messages in ER7 (pipe
//! delimited) encoding without losing information:
//!
//! - Serializing an unmodified message reproduces the input byte for byte,
//!   including custom delimiters, Z-segments, trailing empty fields, escape
//!   sequences, non-UTF-8 bytes and CR/LF/CRLF line endings.
//! - Edits rewrite only the values they touch.
//! - Values are addressed with HL7 paths such as `PID-5.1`, `OBX[2]-5` or
//!   `PID-3[2].4`.
//! - Text is decoded and encoded with the character set declared in MSH-18.
//! - Acknowledgments (`ACK`) are built from the inbound message, in original
//!   or enhanced mode.
//!
//! The crate performs no I/O and never reads the clock; transport framing
//! lives in `oxim-mllp`.
//!
//! ```
//! use oxim_hl7::{AckCode, AckOptions, Message, build_ack};
//!
//! let input = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01^ORU_R01|MSG0001|P|2.5.1\r\
//! PID|1||12345^^^HOSP^MR||Doe^Jane\r\
//! OBX|1|NM|GLU^Glucose^L||5.4|mmol/L|3.9-6.1|N|||F\r";
//!
//! let mut message = Message::parse(input)?;
//! assert_eq!(message.to_bytes(), input);
//! assert_eq!(message.get("PID-5.1").unwrap(), "Doe");
//! assert_eq!(message.get("OBX-5").unwrap().to_string_lossy(), "5.4");
//!
//! message.set("PID-5.2", "Janet")?;
//! assert!(message.to_bytes().windows(9).any(|w| w == b"Doe^Janet"));
//!
//! let ack = build_ack(
//!     &message,
//!     &AckOptions {
//!         code: AckCode::ApplicationAccept,
//!         control_id: "ACK0001",
//!         timestamp: "20260929120001",
//!         text: None,
//!         error: None,
//!     },
//! )?;
//! assert_eq!(ack.get("MSA-2").unwrap(), "MSG0001");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod ack;
mod charset;
mod delimiters;
mod error;
mod escape;
mod message;
mod path;
mod segment;
mod value;

pub use ack::{
    AckBuildError, AckCode, AckCondition, AckError, AckMode, AckOptions, ErrorLocation, Severity,
    build_ack, requested_mode,
};
pub use charset::encoding_for_charset;
pub use delimiters::Delimiters;
pub use error::{CharsetError, EscapeError, ParseError, PathError};
pub use escape::{escape, unescape};
pub use message::{
    InvalidVersion, Message, MessageType, ParseOptions, SegmentTerminators, Version,
};
pub use path::{FieldPath, MAX_INDEX, Path};
pub use segment::{LineEnding, SegmentMut, SegmentRef};
pub use value::{Level, Parts, Value};

/// Re-exported so callers can name encodings without depending on
/// `encoding_rs` directly.
pub use encoding_rs::Encoding;
