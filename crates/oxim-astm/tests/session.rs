//! Scenario tests for LIS01 link sessions, including two sessions talking to
//! each other.

use std::time::{Duration, Instant};

use oxim_astm::frame::{ACK, ENQ, EOT, Frame, NAK, STX, split_message};
use oxim_astm::session::{AbortReason, Event, Output, Role, Session, SessionConfig};

const MESSAGE: &[u8] =
    b"H|\\^&|||ANALYZER\rP|1\rO|1|SMP001||^^^GLU\rR|1|^^^GLU|5.4|mmol/L\rL|1|N\r";

fn drain(session: &mut Session) -> Vec<Output> {
    std::iter::from_fn(|| session.poll_output()).collect()
}

fn transmitted(outputs: &[Output]) -> Vec<u8> {
    outputs
        .iter()
        .filter_map(|output| match output {
            Output::Transmit(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn config(role: Role) -> SessionConfig {
    let mut config = SessionConfig::default();
    config.role = role;
    config
}

#[test]
fn receives_a_message_frame_by_frame() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    session.handle_input(&[ENQ], t0);
    assert_eq!(drain(&mut session), [Output::Transmit(vec![ACK])]);

    let frames = split_message(MESSAGE, 1, 240).unwrap();
    for frame in &frames {
        session.handle_input(&frame.encode(), t0);
        assert_eq!(drain(&mut session), [Output::Transmit(vec![ACK])]);
    }
    session.handle_input(&[EOT], t0);
    assert_eq!(drain(&mut session), [Output::Received(MESSAGE.to_vec())]);
    assert!(session.is_idle());
}

#[test]
fn receives_frames_split_across_reads_and_with_noise() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    let mut stream = vec![ENQ];
    for frame in split_message(MESSAGE, 1, 240).unwrap() {
        stream.extend(frame.encode());
        stream.extend(b"\r\n");
    }
    stream.push(EOT);
    for byte in stream {
        session.handle_input(&[byte], t0);
    }
    let outputs = drain(&mut session);
    assert!(outputs.contains(&Output::Received(MESSAGE.to_vec())));
    assert!(!transmitted(&outputs).contains(&NAK));
}

#[test]
fn refuses_bad_frames_and_accepts_retransmissions() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    session.handle_input(&[ENQ], t0);
    drain(&mut session);

    let frame = Frame::new(1, b"H|\\^&\r".to_vec(), true).unwrap();
    let mut corrupted = frame.encode();
    corrupted[3] = b'X';
    session.handle_input(&corrupted, t0);
    let outputs = drain(&mut session);
    assert!(matches!(outputs[0], Output::Event(Event::InvalidFrame(_))));
    assert_eq!(outputs[1], Output::Transmit(vec![NAK]));

    session.handle_input(&frame.encode(), t0);
    assert_eq!(drain(&mut session), [Output::Transmit(vec![ACK])]);

    // The sender lost our ACK and repeats the frame.
    session.handle_input(&frame.encode(), t0);
    assert_eq!(
        drain(&mut session),
        [
            Output::Event(Event::DuplicateFrame { number: 1 }),
            Output::Transmit(vec![ACK])
        ]
    );

    // A frame from the future is refused.
    let skipped = Frame::new(3, b"L|1\r".to_vec(), true).unwrap();
    session.handle_input(&skipped.encode(), t0);
    assert_eq!(
        drain(&mut session),
        [
            Output::Event(Event::UnexpectedFrameNumber {
                expected: 2,
                received: 3
            }),
            Output::Transmit(vec![NAK])
        ]
    );

    let last = Frame::new(2, b"L|1\r".to_vec(), true).unwrap();
    session.handle_input(&last.encode(), t0);
    session.handle_input(&[EOT], t0);
    let outputs = drain(&mut session);
    assert!(outputs.contains(&Output::Received(b"H|\\^&\rL|1\r".to_vec())));
}

#[test]
fn discards_incomplete_and_stale_receptions() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    session.handle_input(&[ENQ], t0);
    let intermediate = Frame::new(1, b"H|\\^&".to_vec(), false).unwrap();
    session.handle_input(&intermediate.encode(), t0);
    session.handle_input(&[EOT], t0);
    let outputs = drain(&mut session);
    assert!(outputs.contains(&Output::Event(Event::IncompleteMessage { discarded: 5 })));

    session.handle_input(&[ENQ], t0);
    session.handle_input(&intermediate.encode(), t0);
    drain(&mut session);
    assert_eq!(session.poll_timeout(), Some(t0 + secs(30)));
    session.handle_timeout(t0 + secs(30));
    assert_eq!(
        drain(&mut session),
        [Output::Event(Event::ReceiveTimeout { discarded: 5 })]
    );
    assert_eq!(session.poll_timeout(), None);
}

#[test]
fn refuses_enq_while_busy() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    session.set_busy(true);
    session.handle_input(&[ENQ], t0);
    assert_eq!(
        drain(&mut session),
        [
            Output::Transmit(vec![NAK]),
            Output::Event(Event::BusyRefused)
        ]
    );
}

#[test]
fn sends_a_message() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    let id = session.send(MESSAGE.to_vec(), t0).unwrap();
    assert_eq!(drain(&mut session), [Output::Transmit(vec![ENQ])]);
    assert_eq!(session.poll_timeout(), Some(t0 + secs(15)));

    session.handle_input(&[ACK], t0);
    let frames = split_message(MESSAGE, 1, 240).unwrap();
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(
            drain(&mut session),
            [Output::Transmit(frame.encode())],
            "frame {i}"
        );
        session.handle_input(&[ACK], t0);
    }
    assert_eq!(
        drain(&mut session),
        [Output::Delivered(id), Output::Transmit(vec![EOT])]
    );
    assert!(session.is_idle());
}

#[test]
fn retransmits_then_aborts() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    let id = session.send(b"H|\\^&\rL|1\r".to_vec(), t0).unwrap();
    session.handle_input(&[ACK], t0);
    drain(&mut session);
    for attempt in 1..=6 {
        session.handle_input(&[NAK], t0);
        let outputs = drain(&mut session);
        assert_eq!(outputs[0], Output::Event(Event::Retransmission { attempt }));
        assert!(matches!(&outputs[1], Output::Transmit(bytes) if bytes[0] == STX));
    }
    session.handle_input(&[NAK], t0);
    assert_eq!(
        drain(&mut session),
        [
            Output::Transmit(vec![EOT]),
            Output::Aborted {
                id,
                reason: AbortReason::TooManyRetransmissions
            }
        ]
    );
}

#[test]
fn aborts_when_the_receiver_goes_quiet() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    let id = session.send(b"H|\\^&\rL|1\r".to_vec(), t0).unwrap();
    session.handle_input(&[ACK], t0);
    drain(&mut session);
    session.handle_timeout(t0 + secs(14));
    assert!(drain(&mut session).is_empty());
    session.handle_timeout(t0 + secs(15));
    assert_eq!(
        drain(&mut session),
        [
            Output::Transmit(vec![EOT]),
            Output::Aborted {
                id,
                reason: AbortReason::ReplyTimeout
            }
        ]
    );
}

#[test]
fn retries_the_link_request_after_refusal_or_silence() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    session.send(b"H|\\^&\rL|1\r".to_vec(), t0).unwrap();
    drain(&mut session);
    session.handle_input(&[NAK], t0);
    assert_eq!(drain(&mut session), [Output::Event(Event::EnqRefused)]);
    assert_eq!(session.poll_timeout(), Some(t0 + secs(10)));
    session.handle_timeout(t0 + secs(10));
    assert_eq!(drain(&mut session), [Output::Transmit(vec![ENQ])]);

    session.handle_timeout(t0 + secs(25));
    assert_eq!(
        drain(&mut session),
        [
            Output::Transmit(vec![EOT]),
            Output::Event(Event::EnqUnanswered)
        ]
    );
    assert_eq!(session.queued(), 1);
}

#[test]
fn host_yields_on_contention() {
    let t0 = Instant::now();
    let mut host = Session::new(config(Role::Host));
    host.send(b"H|\\^&\rL|1\r".to_vec(), t0).unwrap();
    drain(&mut host);
    host.handle_input(&[ENQ], t0);
    assert_eq!(drain(&mut host), [Output::Event(Event::Contention)]);
    assert_eq!(host.poll_timeout(), Some(t0 + secs(20)));
    // The instrument asks again one second later and the host accepts.
    host.handle_input(&[ENQ], t0 + secs(1));
    assert_eq!(drain(&mut host), [Output::Transmit(vec![ACK])]);
}

#[test]
fn instrument_keeps_priority_on_contention() {
    let t0 = Instant::now();
    let mut instrument = Session::new(config(Role::Instrument));
    instrument.send(b"H|\\^&\rL|1\r".to_vec(), t0).unwrap();
    drain(&mut instrument);
    instrument.handle_input(&[ENQ], t0);
    assert_eq!(drain(&mut instrument), [Output::Event(Event::Contention)]);
    assert_eq!(instrument.poll_timeout(), Some(t0 + secs(1)));
    instrument.handle_timeout(t0 + secs(1));
    assert_eq!(drain(&mut instrument), [Output::Transmit(vec![ENQ])]);
}

#[test]
fn honors_receiver_interrupts() {
    let t0 = Instant::now();
    let mut session = Session::new(SessionConfig::default());
    let id = session.send(b"H|\\^&\rL|1\r".to_vec(), t0).unwrap();
    session.send(b"H|\\^&\rL|1\r".to_vec(), t0).unwrap();
    session.handle_input(&[ACK], t0);
    drain(&mut session);
    session.handle_input(&[EOT], t0);
    let outputs = drain(&mut session);
    assert_eq!(outputs[0], Output::Event(Event::InterruptRequested));
    session.handle_input(&[ACK], t0);
    assert_eq!(
        drain(&mut session),
        [Output::Delivered(id), Output::Transmit(vec![EOT])]
    );
    // The second message waits so the other side can use the link.
    assert_eq!(session.poll_timeout(), Some(t0 + secs(15)));
    session.handle_input(&[ENQ], t0 + secs(2));
    assert_eq!(drain(&mut session), [Output::Transmit(vec![ACK])]);
}

#[test]
fn rejects_messages_that_cannot_be_framed() {
    let mut session = Session::new(SessionConfig::default());
    let now = Instant::now();
    assert!(session.send(Vec::new(), now).is_err());
    assert!(session.send(b"H|\\^&\r\nL|1\r\n".to_vec(), now).is_err());
}

/// Connects two sessions through perfect links and runs them to completion.
fn exchange(
    host: &mut Session,
    instrument: &mut Session,
    t0: Instant,
) -> (Vec<Output>, Vec<Output>) {
    let mut host_log = Vec::new();
    let mut instrument_log = Vec::new();
    for _ in 0..1000 {
        let from_host = drain(host);
        let from_instrument = drain(instrument);
        if from_host.is_empty() && from_instrument.is_empty() {
            break;
        }
        instrument.handle_input(&transmitted(&from_host), t0);
        host.handle_input(&transmitted(&from_instrument), t0);
        host_log.extend(from_host);
        instrument_log.extend(from_instrument);
    }
    (host_log, instrument_log)
}

#[test]
fn two_sessions_exchange_messages_in_both_directions() {
    let t0 = Instant::now();
    let mut host = Session::new(config(Role::Host));
    let mut instrument = Session::new(config(Role::Instrument));
    let long_record = format!("H|\\^&\rC|1|I|{}|G\rL|1\r", "x".repeat(700));
    instrument.send(MESSAGE.to_vec(), t0).unwrap();
    instrument
        .send(long_record.clone().into_bytes(), t0)
        .unwrap();
    let (host_log, instrument_log) = exchange(&mut host, &mut instrument, t0);
    let received: Vec<_> = host_log
        .iter()
        .filter_map(|o| match o {
            Output::Received(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(received, [MESSAGE.to_vec(), long_record.into_bytes()]);
    assert_eq!(
        instrument_log
            .iter()
            .filter(|o| matches!(o, Output::Delivered(_)))
            .count(),
        2
    );

    host.send(b"H|\\^&\rO|1|SMP002||^^^HGB\rL|1\r".to_vec(), t0)
        .unwrap();
    let (_, instrument_log) = exchange(&mut host, &mut instrument, t0);
    assert!(instrument_log.contains(&Output::Received(
        b"H|\\^&\rO|1|SMP002||^^^HGB\rL|1\r".to_vec()
    )));
}
