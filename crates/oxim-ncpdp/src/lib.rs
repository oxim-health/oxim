//! Lossless NCPDP Telecommunication Standard transmissions.
//!
//! `oxim-ncpdp` reads, edits and writes pharmacy claim and service
//! transmissions in the NCPDP Telecommunication Standard, version D.0 (the
//! 5.1 framing is compatible):
//!
//! - The fixed-width header (56 bytes for requests, 31 for responses) is
//!   read field by field ([`REQUEST_HEADER`], [`RESPONSE_HEADER`]).
//! - Segments start with the segment separator (`0x1E`), transaction groups
//!   with the group separator (`0x1D`) and every field with the field
//!   separator (`0x1C`) followed by its two-character identifier.
//! - Serializing an unmodified transmission reproduces the input byte for
//!   byte.
//! - Values are addressed with paths such as `HDR.A1`, `AM07.D2`,
//!   `AM07[2].D2` or `AM08.E4[2]`.
//! - [`build_response`] answers a request with accepted, paid or rejected
//!   transactions and reject codes.
//!
//! NCPDP SCRIPT (XML e-prescribing) is a different standard and out of
//! scope; SCRIPT messages can be handled as XML documents. Transport
//! framing must be removed before parsing. The crate performs no I/O.
//!
//! ```
//! use oxim_ncpdp::Transmission;
//!
//! let mut input = b"999999D0B1PCN1234567101SYNTHPHARM01   20260929SYNTHVEND1".to_vec();
//! input.extend_from_slice(b"\x1d\x1e\x1cAM07\x1cEM1\x1cD2000000123456");
//! let mut transmission = Transmission::parse(&input)?;
//! assert_eq!(transmission.to_bytes(), input);
//! assert_eq!(transmission.get("HDR.A3").as_deref(), Some("B1"));
//! assert_eq!(transmission.get("AM07.D2").as_deref(), Some("000000123456"));
//! transmission.set("AM07.D2", "000000654321")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod error;
mod header;
mod path;
mod response;
mod transmission;

pub use error::{ParseError, PathError};
pub use header::{
    HeaderField, HeaderKind, REQUEST_HEADER, REQUEST_HEADER_LEN, RESPONSE_HEADER,
    RESPONSE_HEADER_LEN, segment_name,
};
pub use path::{MAX_INDEX, Path};
pub use response::{
    HeaderStatus, ResponseError, ResponseOptions, TransactionResponse, TransactionStatus,
    build_response,
};
pub use transmission::{
    FIELD_SEPARATOR, GROUP_SEPARATOR, Issue, ParseOptions, SEGMENT_SEPARATOR, SegmentRef,
    Transmission,
};
