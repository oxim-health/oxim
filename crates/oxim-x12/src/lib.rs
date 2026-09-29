//! Lossless ASC X12 interchanges.
//!
//! `oxim-x12` reads, edits and writes X12 EDI (for example `837` claims,
//! `270`/`271` eligibility, `835` remittance) without losing information:
//!
//! - The delimiters come from the `ISA` segment: element separator,
//!   repetition separator (`ISA-11`, version 00402 and later), component
//!   separator (`ISA-16`) and segment terminator.
//! - Serializing an unmodified interchange reproduces the input byte for
//!   byte, including line breaks after segment terminators, a missing final
//!   terminator and whitespace before `ISA`.
//! - Values are addressed with paths such as `NM1[2]-3`, `CLM-5.1` or
//!   `HI-1[2].2`, or with X12 reference designators such as `NM103` and
//!   `CLM05-01`.
//! - [`Interchange::envelope`] describes the `ISA`/`GS`/`ST` structure and
//!   [`Interchange::validate`] reports missing trailers, mismatched control
//!   numbers and wrong counts; strict parsing rejects them.
//! - [`build_ta1`] and [`build_functional_ack`] write `TA1`, `997` and
//!   `999` acknowledgments.
//!
//! X12 has no escape mechanism, so text containing a delimiter cannot be
//! written and is rejected. The crate performs no I/O and never reads the
//! clock; implementation guide rules (loops, situational requirements) are
//! outside its scope.
//!
//! ```
//! use oxim_x12::Interchange;
//!
//! let input = b"ISA*00*          *00*          *ZZ*SUBMITTER      *ZZ*RECEIVER       *260929*1200*^*00501*000000001*0*T*:~\
//! GS*HC*SUBMITTER*RECEIVER*20260929*1200*1*X*005010X222A1~\
//! ST*837*0001*005010X222A1~\
//! NM1*IL*1*DOE*JANE****MI*SYN12345~\
//! SE*3*0001~GE*1*1~IEA*1*000000001~";
//!
//! let mut interchange = Interchange::parse(input)?;
//! assert_eq!(interchange.to_bytes(), input);
//! assert_eq!(interchange.get("NM1-3").unwrap(), "DOE");
//! assert_eq!(interchange.get("NM109").unwrap(), "SYN12345");
//! assert!(interchange.validate().is_empty());
//!
//! interchange.set("NM1-4", "JANET")?;
//! assert_eq!(interchange.get("NM104").unwrap(), "JANET");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod ack;
mod delimiters;
mod envelope;
mod error;
mod interchange;
mod path;
mod segment;
mod value;

pub use ack::{
    AckCode, AckError, AckHeader, ElementError, FunctionalAckKind, FunctionalAckOptions,
    SegmentError, Ta1Options, TransactionAck, build_functional_ack, build_ta1,
};
pub use delimiters::Delimiters;
pub use envelope::{
    Envelope, GroupEnvelope, InterchangeEnvelope, Issue, IssueKind, TransactionEnvelope,
};
pub use error::{ParseError, PathError};
pub use interchange::{Interchange, ParseOptions};
pub use path::{MAX_INDEX, Path};
pub use segment::{SegmentMut, SegmentRef};
pub use value::{Level, Value};
