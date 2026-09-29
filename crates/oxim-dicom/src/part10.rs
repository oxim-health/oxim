//! DICOM Part 10 objects (PS3.10): a 128-byte preamble, the `DICM`
//! prefix, the file meta information group in Explicit VR Little Endian,
//! then the data set in the transfer syntax the meta information names.
//!
//! OXIM stores and forwards DICOM objects in this form: a `dicom-scp`
//! source submits each received instance as a Part 10 object, and
//! destinations and steps read the transfer syntax from its meta
//! information.

use crate::error::DicomError;
use crate::uids;

const PREAMBLE: usize = 128;
const MAGIC: &[u8; 4] = b"DICM";

/// The file meta information of a Part 10 object.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileMeta {
    /// Media Storage SOP Class UID (0002,0002).
    pub sop_class_uid: String,
    /// Media Storage SOP Instance UID (0002,0003).
    pub sop_instance_uid: String,
    /// Transfer Syntax UID (0002,0010) of the data set.
    pub transfer_syntax: String,
    /// Source Application Entity Title (0002,0016): the AE that wrote the
    /// object.
    pub source_ae_title: Option<String>,
    /// Sending Application Entity Title (0002,0017): the AE the object was
    /// received from.
    pub sending_ae_title: Option<String>,
    /// Receiving Application Entity Title (0002,0018): the AE that received
    /// the object.
    pub receiving_ae_title: Option<String>,
}

/// A Part 10 object split into its file meta information and its data set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part10<'a> {
    /// The file meta information.
    pub meta: FileMeta,
    /// The encoded data set, in the transfer syntax of `meta`.
    pub dataset: &'a [u8],
}

/// Whether an explicit VR uses the long header form (two reserved bytes
/// and a 32-bit length).
pub(crate) fn is_long_vr(vr: [u8; 2]) -> bool {
    matches!(
        &vr,
        b"OB"
            | b"OD"
            | b"OF"
            | b"OL"
            | b"OV"
            | b"OW"
            | b"SQ"
            | b"SV"
            | b"UC"
            | b"UN"
            | b"UR"
            | b"UT"
            | b"UV"
    )
}

/// A text value without its padding.
pub(crate) fn trimmed_text(value: &[u8]) -> String {
    String::from_utf8_lossy(value)
        .trim_end_matches(['\0', ' '])
        .trim_start_matches(' ')
        .to_owned()
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *bytes.get(at)?,
        *bytes.get(at + 1)?,
        *bytes.get(at + 2)?,
        *bytes.get(at + 3)?,
    ]))
}

/// Splits a Part 10 object into its meta information and data set.
///
/// The 128-byte preamble may be missing when the bytes start with `DICM`.
/// The data set starts at the first element after group `0002`; the group
/// length element is not trusted.
pub fn parse(bytes: &[u8]) -> Result<Part10<'_>, DicomError> {
    let mut rest = if bytes.get(PREAMBLE..PREAMBLE + 4) == Some(MAGIC.as_slice()) {
        &bytes[PREAMBLE + 4..]
    } else if bytes.starts_with(MAGIC) {
        &bytes[4..]
    } else {
        return Err(DicomError::invalid(
            "not a DICOM Part 10 object: the DICM prefix is missing",
        ));
    };
    let truncated = || DicomError::invalid("the file meta information is truncated");
    let mut meta = FileMeta::default();
    while le16(rest, 0) == Some(0x0002) {
        let element = le16(rest, 2).ok_or_else(truncated)?;
        let vr = [
            *rest.get(4).ok_or_else(truncated)?,
            *rest.get(5).ok_or_else(truncated)?,
        ];
        if !vr.iter().all(u8::is_ascii_uppercase) {
            return Err(DicomError::invalid(
                "the file meta information must use explicit VR",
            ));
        }
        let (header, length) = if is_long_vr(vr) {
            (12, le32(rest, 8).ok_or_else(truncated)?)
        } else {
            (8, u32::from(le16(rest, 6).ok_or_else(truncated)?))
        };
        if length == u32::MAX {
            return Err(DicomError::invalid(
                "the file meta information has an element of undefined length",
            ));
        }
        let end = usize::try_from(length)
            .ok()
            .and_then(|length| length.checked_add(header))
            .filter(|end| *end <= rest.len())
            .ok_or_else(truncated)?;
        let value = &rest[header..end];
        match element {
            0x0002 => meta.sop_class_uid = trimmed_text(value),
            0x0003 => meta.sop_instance_uid = trimmed_text(value),
            0x0010 => meta.transfer_syntax = trimmed_text(value),
            0x0016 => meta.source_ae_title = Some(trimmed_text(value)),
            0x0017 => meta.sending_ae_title = Some(trimmed_text(value)),
            0x0018 => meta.receiving_ae_title = Some(trimmed_text(value)),
            _ => {}
        }
        rest = &rest[end..];
    }
    if meta.transfer_syntax.is_empty() {
        return Err(DicomError::invalid(
            "the file meta information has no transfer syntax",
        ));
    }
    Ok(Part10 {
        meta,
        dataset: rest,
    })
}

fn put(out: &mut Vec<u8>, element: u16, vr: [u8; 2], value: &[u8]) {
    out.extend_from_slice(&0x0002u16.to_le_bytes());
    out.extend_from_slice(&element.to_le_bytes());
    out.extend_from_slice(&vr);
    if is_long_vr(vr) {
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_le_bytes());
    } else {
        out.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_le_bytes());
    }
    out.extend_from_slice(value);
}

/// A text value cut to `max` bytes and padded to an even length.
pub(crate) fn padded(text: &str, max: usize, pad: u8) -> Vec<u8> {
    let mut value: Vec<u8> = text.bytes().take(max).collect();
    if !value.len().is_multiple_of(2) {
        value.push(pad);
    }
    value
}

/// Encodes a Part 10 object: preamble, prefix, file meta information and
/// data set. The meta information names OXIM as the implementation.
pub fn encode(meta: &FileMeta, dataset: &[u8]) -> Vec<u8> {
    let mut group = Vec::with_capacity(256);
    put(&mut group, 0x0001, *b"OB", &[0, 1]);
    put(
        &mut group,
        0x0002,
        *b"UI",
        &padded(&meta.sop_class_uid, 64, 0),
    );
    put(
        &mut group,
        0x0003,
        *b"UI",
        &padded(&meta.sop_instance_uid, 64, 0),
    );
    put(
        &mut group,
        0x0010,
        *b"UI",
        &padded(&meta.transfer_syntax, 64, 0),
    );
    put(
        &mut group,
        0x0012,
        *b"UI",
        &padded(uids::IMPLEMENTATION_CLASS_UID, 64, 0),
    );
    put(
        &mut group,
        0x0013,
        *b"SH",
        &padded(uids::IMPLEMENTATION_VERSION_NAME, 16, b' '),
    );
    for (element, title) in [
        (0x0016, &meta.source_ae_title),
        (0x0017, &meta.sending_ae_title),
        (0x0018, &meta.receiving_ae_title),
    ] {
        if let Some(title) = title.as_deref().filter(|title| !title.is_empty()) {
            put(&mut group, element, *b"AE", &padded(title, 16, b' '));
        }
    }
    let mut out = Vec::with_capacity(PREAMBLE + 16 + group.len() + dataset.len());
    out.resize(PREAMBLE, 0);
    out.extend_from_slice(MAGIC);
    let group_length = u32::try_from(group.len()).unwrap_or(u32::MAX);
    put(&mut out, 0x0000, *b"UL", &group_length.to_le_bytes());
    out.extend_from_slice(&group);
    out.extend_from_slice(dataset);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> FileMeta {
        FileMeta {
            sop_class_uid: "1.2.840.10008.5.1.4.1.1.7".into(),
            sop_instance_uid: "1.2.3.4.5".into(),
            transfer_syntax: uids::EXPLICIT_VR_LITTLE_ENDIAN.into(),
            source_ae_title: Some("OXIM".into()),
            sending_ae_title: Some("MODALITY".into()),
            receiving_ae_title: None,
        }
    }

    #[test]
    fn round_trips() {
        let dataset = b"\x08\x00\x60\x00CS\x02\x00OT";
        let bytes = encode(&meta(), dataset);
        assert_eq!(&bytes[128..132], b"DICM");
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.meta, meta());
        assert_eq!(parsed.dataset, dataset);
        // Without the preamble.
        let parsed = parse(&bytes[128..]).unwrap();
        assert_eq!(parsed.dataset, dataset);
    }

    #[test]
    fn rejects_malformed_meta() {
        assert!(parse(b"").is_err());
        assert!(parse(b"not dicom at all").is_err());
        let bytes = encode(&meta(), b"");
        // Truncated inside the meta group.
        assert!(parse(&bytes[..150]).is_err());
        // No transfer syntax.
        let mut no_ts = meta();
        no_ts.transfer_syntax.clear();
        assert!(parse(&encode(&no_ts, b"")).is_err());
        // A huge declared length.
        let mut huge = b"DICM\x02\x00\x01\x00OB\x00\x00".to_vec();
        huge.extend_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(parse(&huge).is_err());
    }
}
