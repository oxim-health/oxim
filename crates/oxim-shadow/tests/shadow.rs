//! Shadow mode end to end: a synthetic capture of a Mirth channel is
//! replayed through the equivalent OXIM channel and compared.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use oxim_core::{ChannelConfig, Registry};
use oxim_shadow::{CompareOptions, DestinationCapture, Framing, ShadowOptions, Traffic, shadow};

/// An Ethernet/IPv4/TCP frame.
fn frame(
    source: ([u8; 4], u16),
    destination: ([u8; 4], u16),
    sequence: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = vec![0u8; 12];
    out.extend_from_slice(&[0x08, 0x00]);
    let total = u16::try_from(40 + payload.len()).unwrap();
    out.extend_from_slice(&[0x45, 0]);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0x40, 0, 64, 6, 0, 0]);
    out.extend_from_slice(&source.0);
    out.extend_from_slice(&destination.0);
    out.extend_from_slice(&source.1.to_be_bytes());
    out.extend_from_slice(&destination.1.to_be_bytes());
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0x50, 0x18, 0xff, 0xff, 0, 0, 0, 0]);
    out.extend_from_slice(payload);
    out
}

fn pcap(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0];
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&65535u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    for (i, frame) in frames.iter().enumerate() {
        out.extend_from_slice(&1_790_000_000u32.to_le_bytes());
        out.extend_from_slice(&(u32::try_from(i).unwrap() * 1000).to_le_bytes());
        let length = u32::try_from(frame.len()).unwrap();
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(frame);
    }
    out
}

fn result(control: &str, time: &str, sender: &str, name: &str) -> Vec<u8> {
    format!(
        "MSH|^~\\&|{sender}|LAB|LIS|HOSP|{time}||ORU^R01^ORU_R01|{control}|P|2.5.1\rPID|1||P1001||{name}\rOBX|1|NM|GLU^Glucose||5.4|mmol/L\r"
    )
    .into_bytes()
}

const ANALYZER: [u8; 4] = [10, 0, 0, 5];
const MIRTH: [u8; 4] = [10, 0, 0, 1];
const LIS: [u8; 4] = [10, 0, 0, 9];

#[tokio::test(flavor = "multi_thread")]
async fn compares_oxim_with_the_captured_mirth_output() {
    // The analyzer sends two results to Mirth on port 6661; Mirth relays
    // them to the LIS on port 6662 with MSH-3 set to MIRTH-OUT, new
    // timestamps and control ids, and (a Mirth quirk) the second patient
    // name upper-cased.
    let inbound = [
        result("A1", "20260929120000", "CHEM", "Doe^Jane"),
        result("A2", "20260929120100", "CHEM", "Roe^Richard"),
    ];
    let outbound = [
        result("M1", "20260929120001", "MIRTH-OUT", "Doe^Jane"),
        result("M2", "20260929120101", "MIRTH-OUT", "ROE^RICHARD"),
    ];
    let mut frames = Vec::new();
    let mut client_seq = 1000u32;
    let mut mirth_seq = 5000u32;
    for (message, relayed) in inbound.iter().zip(outbound.iter()) {
        let framed = oxim_mllp::encode(message).unwrap();
        // Split the first frame over two segments to exercise reassembly.
        let (a, b) = framed.split_at(10);
        frames.push(frame((ANALYZER, 40000), (MIRTH, 6661), client_seq, a));
        frames.push(frame((ANALYZER, 40000), (MIRTH, 6661), client_seq + 10, b));
        client_seq += u32::try_from(framed.len()).unwrap();
        let framed = oxim_mllp::encode(relayed).unwrap();
        frames.push(frame((MIRTH, 50000), (LIS, 6662), mirth_seq, &framed));
        mirth_seq += u32::try_from(framed.len()).unwrap();
    }
    let capture = pcap(&frames);

    let channel = ChannelConfig::from_yaml(
        "id: results-to-lis
source:
  type: mllp
  data_type: hl7v2
  settings: {listen: '0.0.0.0:6661'}
transformers:
  - {type: set, path: MSH-3, value: MIRTH-OUT}
destinations:
  - id: lis
    type: mllp
    settings: {target: 'lis.example.org:6662'}
",
    )
    .unwrap();
    let options = ShadowOptions {
        inbound_port: 6661,
        inbound_framing: Framing::Mllp,
        destinations: vec![DestinationCapture {
            destination: "lis".into(),
            port: 6662,
            framing: Framing::Mllp,
        }],
        compare: CompareOptions {
            ignore: vec!["MSH-7".into(), "MSH-10".into()],
            key: None,
            show_values: false,
        },
        timeout: Duration::from_secs(10),
    };
    let traffic = Traffic::from_pcap(&capture, &options).unwrap();
    assert_eq!(traffic.inbound.len(), 2);
    assert_eq!(traffic.outbound["lis"].len(), 2);

    let report = shadow(&channel, Registry::new(), &traffic, &options)
        .await
        .unwrap();
    assert_eq!(report.inbound_messages, 2);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let lis = &report.destinations[0];
    assert_eq!((lis.mirth_messages, lis.oxim_messages), (2, 2));
    assert_eq!(lis.identical, 1);
    assert_eq!(lis.different.len(), 1);
    assert_eq!(lis.different[0].fields[0].path, "PID-5");
    // Values stay hidden unless asked for.
    assert_eq!(lis.different[0].fields[0].mirth, "(11 bytes)");
    assert!(!report.is_identical());
    let markdown = report.to_markdown();
    assert!(
        markdown.contains("| lis | 2 | 2 | 1 | 1 | 0 | 0 |"),
        "{markdown}"
    );
    assert!(markdown.contains("`PID-5`"), "{markdown}");
    assert!(!markdown.contains("RICHARD"), "{markdown}");
    let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
    assert_eq!(json["destinations"][0]["identical"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_destinations_are_rejected() {
    let channel = ChannelConfig::from_yaml(
        "id: c\nsource: {type: mllp, data_type: hl7v2, settings: {listen: '0.0.0.0:1'}}\n",
    )
    .unwrap();
    let options = ShadowOptions {
        inbound_port: 1,
        inbound_framing: Framing::Mllp,
        destinations: vec![DestinationCapture {
            destination: "nowhere".into(),
            port: 2,
            framing: Framing::Mllp,
        }],
        compare: CompareOptions::default(),
        timeout: Duration::from_secs(1),
    };
    let error = shadow(&channel, Registry::new(), &Traffic::default(), &options)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("nowhere"));
}
