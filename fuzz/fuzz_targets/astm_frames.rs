#![no_main]

//! Feeds arbitrary bytes to the LIS01 frame decoder and to link sessions in
//! both roles, in arbitrary chunks and with timeouts in between.

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use oxim_astm::frame::{Decoded, decode_frame};
use oxim_astm::session::{Output, Role, Session, SessionConfig};

fuzz_target!(|input: (Vec<u8>, Vec<u16>, bool, bool)| {
    let (stream, cuts, instrument, queue_message) = input;

    let mut rest = stream.as_slice();
    while !rest.is_empty() {
        match decode_frame(rest, 240) {
            Decoded::Incomplete => break,
            Decoded::Frame { len, .. } | Decoded::Invalid { len, .. } => {
                assert!(len >= 1 && len <= rest.len());
                rest = &rest[len..];
            }
        }
    }

    let mut config = SessionConfig::default();
    config.role = if instrument {
        Role::Instrument
    } else {
        Role::Host
    };
    config.max_message_len = 4096;
    let mut session = Session::new(config);
    let t0 = Instant::now();
    if queue_message {
        let _ = session.send(b"H|\\^&\rL|1\r".to_vec(), t0);
    }
    let mut cuts: Vec<usize> = cuts
        .into_iter()
        .map(|cut| usize::from(cut) % (stream.len() + 1))
        .collect();
    cuts.sort_unstable();
    let mut start = 0;
    for (step, cut) in cuts.into_iter().chain([stream.len()]).enumerate() {
        let now = t0 + Duration::from_secs(step as u64 * 7);
        session.handle_input(&stream[start..cut], now);
        start = cut;
        session.handle_timeout(now);
        while let Some(output) = session.poll_output() {
            if let Output::Received(text) = output {
                assert!(text.len() <= 4096);
            }
        }
    }
});
