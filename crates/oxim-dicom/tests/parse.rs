//! Parsing untrusted bytes never panics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::Synthetic;
use oxim_dicom::dimse::Command;
use oxim_dicom::part10::{self, FileMeta};
use oxim_dicom::{DicomObject, uids};
use proptest::prelude::*;

/// Part 10 bytes with valid meta information and the given data set.
fn with_meta(dataset: &[u8], transfer_syntax: &str) -> Vec<u8> {
    part10::encode(
        &FileMeta {
            sop_class_uid: common::CT_IMAGE_STORAGE.into(),
            sop_instance_uid: "1.2.3".into(),
            transfer_syntax: transfer_syntax.into(),
            ..FileMeta::default()
        },
        dataset,
    )
}

const SYNTAXES: [&str; 4] = [
    uids::IMPLICIT_VR_LITTLE_ENDIAN,
    uids::EXPLICIT_VR_LITTLE_ENDIAN,
    uids::EXPLICIT_VR_BIG_ENDIAN,
    "1.2.840.10008.1.2.4.50",
];

/// One loosely structured element: tag, VR, declared length and value.
fn element() -> impl Strategy<Value = Vec<u8>> {
    let vrs: Vec<&'static [u8; 2]> = vec![
        b"CS", b"LO", b"UI", b"US", b"UL", b"FD", b"OB", b"OW", b"SQ", b"UN", b"PN", b"DA", b"XX",
    ];
    (
        prop_oneof![
            Just(0x0008u16),
            Just(0x0010),
            Just(0x0020),
            Just(0x0009),
            Just(0x7FE0),
            Just(0xFFFE),
            any::<u16>()
        ],
        prop_oneof![
            Just(0x0010u16),
            Just(0x0060),
            Just(0x1140),
            Just(0xE000),
            Just(0xE00D),
            Just(0xE0DD),
            any::<u16>()
        ],
        proptest::sample::select(vrs),
        prop_oneof![
            Just(None),
            Just(Some(u32::MAX)),
            any::<u32>().prop_map(Some)
        ],
        proptest::collection::vec(any::<u8>(), 0..24),
    )
        .prop_map(|(group, element, vr, length, value)| {
            let mut out = Vec::new();
            out.extend_from_slice(&group.to_le_bytes());
            out.extend_from_slice(&element.to_le_bytes());
            out.extend_from_slice(vr);
            let length = length.unwrap_or(value.len() as u32);
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&value);
            out
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = part10::parse(&bytes);
        let _ = DicomObject::parse(&bytes);
        let _ = Command::decode(&bytes);
        for syntax in SYNTAXES {
            let _ = DicomObject::parse(&with_meta(&bytes, syntax));
        }
    }

    #[test]
    fn structured_garbage_never_panics(
        elements in proptest::collection::vec(element(), 0..12),
        syntax in proptest::sample::select(SYNTAXES.to_vec()),
    ) {
        let dataset = elements.concat();
        if let Ok(object) = DicomObject::parse(&with_meta(&dataset, syntax)) {
            // Whatever parses must also encode.
            let _ = object.to_bytes();
        }
    }

    #[test]
    fn corrupted_objects_never_panic(
        position in any::<proptest::sample::Index>(),
        byte in any::<u8>(),
        truncate in any::<bool>(),
        syntax in proptest::sample::select(SYNTAXES[..3].to_vec()),
    ) {
        let mut bytes = Synthetic::ct(1).with_transfer_syntax(syntax).bytes();
        let at = position.index(bytes.len());
        if truncate {
            bytes.truncate(at);
        } else {
            bytes[at] = byte;
        }
        if let Ok(object) = DicomObject::parse(&bytes) {
            let _ = object.to_bytes();
        }
    }
}

#[test]
fn synthetic_objects_round_trip() {
    for syntax in SYNTAXES[..3].iter().copied() {
        let bytes = Synthetic::ct(1).with_transfer_syntax(syntax).bytes();
        let object = DicomObject::parse(&bytes).unwrap();
        let again = object.to_bytes().unwrap();
        let reparsed = DicomObject::parse(&again).unwrap();
        assert_eq!(
            common::text(&reparsed, "PatientID"),
            common::text(&object, "PatientID")
        );
    }
}
