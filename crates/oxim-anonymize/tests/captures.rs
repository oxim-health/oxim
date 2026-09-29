//! The example captures of `profiles/` anonymize into captures that still
//! decode frame by frame and keep their results.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use oxim_anonymize::{Anonymizer, Options, capture::anonymize_capture};
use oxim_astm::frame::{Decoded, decode_frame};
use oxim_capture::{Capture, Direction, Event, Header, Protocol, Record, Transport, now};

fn fixture(profile: &str, file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles")
        .join(profile)
        .join("fixtures")
        .join(file)
}

fn anonymizer() -> Anonymizer {
    let options = Options {
        free_text: true,
        ..Options::default()
    };
    Anonymizer::new(
        options,
        Some(b"0123456789abcdef0123456789abcdef"),
        Some(-100),
    )
    .unwrap()
}

fn text(capture: &Capture, direction: Direction) -> String {
    String::from_utf8_lossy(&capture.stream(1, direction)).into_owned()
}

/// The data records' directions and the positions of open/close events.
fn shape(capture: &Capture) -> Vec<(Event, Option<Direction>)> {
    capture
        .records
        .iter()
        .map(|r| (r.event, r.direction))
        .collect()
}

#[test]
fn lis01_captures_keep_valid_frames() {
    let original = Capture::load(fixture("generic-astm-analyzer", "results.oximcap")).unwrap();
    let a = anonymizer();
    let mut report = a.report();
    let anonymized = anonymize_capture(&a, &original, &mut report);
    assert!(anonymized.header.anonymized);
    assert_eq!(shape(&anonymized), shape(&original));
    assert_eq!(report.messages, 2);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let before = text(&original, Direction::DeviceToHost);
    let after = text(&anonymized, Direction::DeviceToHost);
    for secret in [
        "PAT000001",
        "PAT000002",
        "SMP000001",
        "SMP000002",
        "19800101",
    ] {
        assert!(before.contains(secret), "the fixture changed: {secret}");
        assert!(!after.contains(secret), "{secret} survived");
    }
    // Every frame decodes with a correct checksum, and results are intact.
    let stream = anonymized.stream(1, Direction::DeviceToHost);
    let mut position = 0;
    let mut frames = 0;
    let mut message = Vec::new();
    while position < stream.len() {
        if stream[position] != 0x02 {
            position += 1;
            continue;
        }
        match decode_frame(&stream[position..], 240) {
            Decoded::Frame { frame, len } => {
                frames += 1;
                message.extend_from_slice(frame.text());
                position += len;
            }
            other => panic!("frame {frames} does not decode: {other:?}"),
        }
    }
    assert!(frames >= 10, "{frames}");
    let results = String::from_utf8(message).unwrap();
    // Result records keep everything before their (shifted) dates.
    let mut checked = 0;
    for line in before.split('\r') {
        let Some(start) = line.find("R|") else {
            continue;
        };
        let values: Vec<&str> = line[start..].split('|').take(11).collect();
        if values.len() == 11 {
            assert!(results.contains(&values.join("|")), "{line}");
            checked += 1;
        }
    }
    assert!(checked >= 5, "{checked}");
    // The host's acknowledgments are untouched.
    assert_eq!(
        anonymized.stream(1, Direction::HostToDevice),
        original.stream(1, Direction::HostToDevice)
    );
    // Reading the result back gives an identical capture.
    assert_eq!(
        Capture::from_slice(&anonymized.to_bytes()).unwrap(),
        anonymized
    );
}

#[test]
fn mllp_and_poct_captures_are_anonymized() {
    let a = anonymizer();
    let mut report = a.report();
    let original = Capture::load(fixture("generic-ihe-law-analyzer", "results.oximcap")).unwrap();
    let anonymized = anonymize_capture(&a, &original, &mut report);
    assert_eq!(shape(&anonymized), shape(&original));
    let after = text(&anonymized, Direction::DeviceToHost);
    for secret in ["PAT000101", "Test^Robin", "SMP000101", "19800101"] {
        assert!(!after.contains(secret), "{secret} survived: {after}");
    }
    assert!(after.starts_with('\u{b}') && after.ends_with("\u{1c}\r"));
    assert!(after.contains("|5.40|mmol/L|"), "{after}");
    let hl7 = after
        .trim_start_matches('\u{b}')
        .trim_end_matches("\u{1c}\r");
    oxim_hl7::Message::parse(hl7.as_bytes()).unwrap();

    let original = Capture::load(fixture("generic-poct1a-device", "upload.oximcap")).unwrap();
    let anonymized = anonymize_capture(&a, &original, &mut report);
    assert_eq!(shape(&anonymized), shape(&original));
    let after = text(&anonymized, Direction::DeviceToHost);
    for secret in ["SIM0000", "SIM0001", "sim-operator"] {
        assert!(!after.contains(secret), "{secret} survived");
    }
    assert!(after.contains("<OBS.value V=\"7.35\""), "{after}");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn split_frames_and_unknown_streams() {
    // An MLLP frame split over two reads ends up whole in the second record;
    // an incomplete frame is dropped; an unknown protocol is kept with a
    // warning.
    let message = b"\x0bMSH|^~\\&|A|B|C|D|20260929120000||ORU^R01|1|P|2.5.1\rPID|1||PAT9^^^H^MR||Doe^Jane\r\x1c\r";
    let mut capture = Capture::new(Header::new(Transport::Tcp, now()));
    capture.header.protocol = Some(Protocol::Hl7v2Mllp);
    let data = |connection, direction, bytes: &[u8]| {
        Record::data(now(), connection, direction, Transport::Tcp, bytes.to_vec())
    };
    capture.records = vec![
        data(1, Direction::DeviceToHost, &message[..30]),
        data(1, Direction::DeviceToHost, &message[30..]),
        data(
            1,
            Direction::HostToDevice,
            b"\x0bMSH|^~\\&|C|D|A|B|20260929120001||ACK|2|P|2.5.1\rMSA|AA|1\r\x1c\r",
        ),
        data(
            1,
            Direction::DeviceToHost,
            b"\x0bMSH|^~\\&|A|B|C|D|20260929120000||ORU^R01|3|P|2.5.1\rPID|1||PAT9",
        ),
    ];
    let a = anonymizer();
    let mut report = a.report();
    let anonymized = anonymize_capture(&a, &capture, &mut report);
    assert_eq!(anonymized.records.len(), 2, "{:?}", anonymized.records);
    let first = String::from_utf8_lossy(&anonymized.records[0].data).into_owned();
    assert!(
        first.starts_with("\u{b}MSH|") && first.ends_with("\u{1c}\r") && !first.contains("PAT9"),
        "{first}"
    );
    assert_eq!(
        anonymized.records[1].direction,
        Some(Direction::HostToDevice)
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("incomplete MLLP frame")),
        "{:?}",
        report.warnings
    );

    let mut unknown = Capture::new(Header::new(Transport::Tcp, now()));
    unknown.records = vec![data(1, Direction::DeviceToHost, b"PATIENT DOE")];
    let mut report = a.report();
    let kept = anonymize_capture(&a, &unknown, &mut report);
    assert_eq!(kept.records, unknown.records);
    assert!(
        report.warnings[0].contains("unknown protocol"),
        "{:?}",
        report.warnings
    );
}
