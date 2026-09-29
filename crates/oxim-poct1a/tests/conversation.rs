//! Scenario tests for the host side of a POCT1-A conversation.

// Test helpers outside `#[test]` functions fail loudly on purpose.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use oxim_poct1a::builder::{self, Header, directive};
use oxim_poct1a::{
    AckType, BuildError, CloseReason, ConversationError, ConversationState, DataAcknowledgment,
    DeliveryOutcome, Element, HostConfig, HostConversation, Message, MessageKind, Now, Output,
    WriteOptions,
};

const DATETIME: &str = "2026-09-29T12:00:00+03:00";

fn at(base: Instant, seconds: u64) -> Now<'static> {
    Now {
        instant: base + Duration::from_secs(seconds),
        datetime: DATETIME,
    }
}

/// A message as the device would send it.
fn device(message_type: &str, control_id: &str, body: Vec<Element>) -> Message {
    builder::build(
        message_type,
        &Header {
            control_id,
            version_id: "POCT1",
            creation_dttm: DATETIME,
        },
        body,
        &WriteOptions::default(),
    )
    .unwrap()
}

fn hello(control_id: &str) -> Message {
    device(
        "HEL.R01",
        control_id,
        vec![
            Element::new("DEV")
                .child(Element::with_v("DEV.device_id", "D1"))
                .child(Element::with_v("DEV.model_id", "BG-100")),
        ],
    )
}

fn status(control_id: &str, new_observations: u32) -> Message {
    device(
        "DST.R01",
        control_id,
        vec![
            Element::new("DST")
                .child(Element::with_v(
                    "DST.new_observations_qty",
                    new_observations.to_string(),
                ))
                .child(Element::with_v("DST.condition_cd", "R")),
        ],
    )
}

fn observation(control_id: &str) -> Message {
    device(
        "OBS.R01",
        control_id,
        vec![
            Element::new("SVC").child(
                Element::new("PT").child(
                    Element::new("OBS")
                        .child(Element::with_v("OBS.observation_id", "GLU"))
                        .child(Element::with_v("OBS.value", "5.4").attribute("U", "mmol/L")),
                ),
            ),
        ],
    )
}

fn device_ack(control_id: &str, ack_type: &str, acked: &str) -> Message {
    device(
        "ACK.R01",
        control_id,
        vec![
            Element::new("ACK")
                .child(Element::with_v("ACK.type_cd", ack_type))
                .child(Element::with_v("ACK.ack_control_id", acked)),
        ],
    )
}

fn outputs(conversation: &mut HostConversation) -> Vec<Output> {
    std::iter::from_fn(|| conversation.poll_output()).collect()
}

/// Asserts that `output` sends an acknowledgment and returns its content.
fn sent_ack(output: &Output) -> (AckType, String, Option<String>) {
    let Output::Send(message) = output else {
        panic!("expected a message to send, got {output:?}");
    };
    let parsed = Message::parse(message.as_bytes()).unwrap();
    assert_eq!(parsed.kind(), MessageKind::Acknowledgment);
    let info = parsed.ack_info().unwrap();
    (
        info.ack_type.unwrap(),
        info.acked_control_id.unwrap(),
        info.note,
    )
}

fn sent(output: &Output) -> &Message {
    match output {
        Output::Send(message) => message,
        other => panic!("expected a message to send, got {other:?}"),
    }
}

/// Runs hello and status so the conversation is ready. Returns the next
/// host control ID.
fn ready(conversation: &mut HostConversation, base: Instant, new_observations: u32) {
    conversation
        .handle_message(hello("100"), at(base, 1))
        .unwrap();
    conversation
        .handle_message(status("101", new_observations), at(base, 2))
        .unwrap();
    outputs(conversation);
}

#[test]
fn full_session_with_manual_acknowledgment() {
    let base = Instant::now();
    let mut host = HostConversation::new(HostConfig::default(), base).unwrap();
    assert_eq!(host.state(), ConversationState::AwaitingHello);
    assert_eq!(host.poll_timeout(), Some(base + Duration::from_secs(60)));

    host.handle_message(hello("100"), at(base, 1)).unwrap();
    let out = outputs(&mut host);
    assert!(matches!(&out[0], Output::Hello(info) if info.device_id.as_deref() == Some("D1")));
    assert_eq!(sent_ack(&out[1]), (AckType::Accept, "100".into(), None));
    assert_eq!(out.len(), 2);
    assert_eq!(host.state(), ConversationState::AwaitingStatus);
    assert_eq!(host.device().unwrap().model_id.as_deref(), Some("BG-100"));

    host.handle_message(status("101", 2), at(base, 2)).unwrap();
    let out = outputs(&mut host);
    assert!(matches!(&out[0], Output::Status(s) if s.new_observations == Some(2)));
    assert_eq!(sent_ack(&out[1]), (AckType::Accept, "101".into(), None));
    let request = sent(&out[2]);
    assert_eq!(request.kind(), MessageKind::Request);
    assert_eq!(request.value("REQ/REQ.request_cd"), Some("ROBS"));
    assert_eq!(request.control_id(), Some("3"));
    assert_eq!(host.poll_timeout(), Some(base + Duration::from_secs(32)));

    host.handle_message(device_ack("102", "AA", "3"), at(base, 3))
        .unwrap();
    assert_eq!(
        outputs(&mut host),
        [Output::Accepted {
            control_id: "3".into()
        }]
    );
    assert_eq!(host.state(), ConversationState::InTopic);
    assert_eq!(host.topic(), Some("OBS"));

    let first = observation("103");
    host.handle_message(first.clone(), at(base, 4)).unwrap();
    assert_eq!(outputs(&mut host), [Output::Observations(first.clone())]);
    assert_eq!(host.awaiting_acknowledgment(), ["103"]);
    // A retransmission while the application is still storing it.
    host.handle_message(first, at(base, 5)).unwrap();
    assert!(outputs(&mut host).is_empty());

    host.acknowledge("103", DeliveryOutcome::Accepted, at(base, 6))
        .unwrap();
    let out = outputs(&mut host);
    assert_eq!(sent_ack(&out[0]), (AckType::Accept, "103".into(), None));
    assert_eq!(
        host.acknowledge("103", DeliveryOutcome::Accepted, at(base, 6)),
        Err(ConversationError::UnknownDelivery("103".into()))
    );

    host.handle_message(observation("104"), at(base, 7))
        .unwrap();
    outputs(&mut host);
    host.acknowledge(
        "104",
        DeliveryOutcome::Error("storage unavailable".into()),
        at(base, 8),
    )
    .unwrap();
    let out = outputs(&mut host);
    assert_eq!(
        sent_ack(&out[0]),
        (
            AckType::Error,
            "104".into(),
            Some("storage unavailable".into())
        )
    );

    let eot = device(
        "EOT.R01",
        "105",
        vec![Element::new("EOT").child(Element::with_v("EOT.topic_cd", "OBS"))],
    );
    host.handle_message(eot, at(base, 9)).unwrap();
    let out = outputs(&mut host);
    assert_eq!(out[0], Output::TopicEnded("OBS".into()));
    assert_eq!(sent_ack(&out[1]), (AckType::Accept, "105".into(), None));
    assert_eq!(host.state(), ConversationState::Ready);

    host.terminate(at(base, 10)).unwrap();
    let out = outputs(&mut host);
    let end = sent(&out[0]);
    assert_eq!(end.kind(), MessageKind::Terminate);
    let end_id = end.control_id().unwrap().to_owned();

    host.handle_message(device_ack("106", "AA", &end_id), at(base, 11))
        .unwrap();
    assert_eq!(
        outputs(&mut host),
        [
            Output::Accepted { control_id: end_id },
            Output::Closed(CloseReason::HostTerminated)
        ]
    );
    assert!(host.is_closed());
    assert_eq!(host.poll_timeout(), None);
    host.handle_message(observation("107"), at(base, 12))
        .unwrap();
    assert!(outputs(&mut host).is_empty());
}

#[test]
fn times_out_waiting_for_hello() {
    let base = Instant::now();
    let mut host = HostConversation::new(HostConfig::default(), base).unwrap();
    host.handle_timeout(at(base, 59)).unwrap();
    assert!(outputs(&mut host).is_empty());
    host.handle_timeout(at(base, 60)).unwrap();
    assert_eq!(
        outputs(&mut host),
        [Output::Closed(CloseReason::HelloTimeout)]
    );
}

#[test]
fn rejects_unexpected_messages() {
    let base = Instant::now();
    let mut host = HostConversation::new(HostConfig::default(), base).unwrap();

    host.handle_message(observation("1"), at(base, 1)).unwrap();
    let out = outputs(&mut host);
    assert_eq!(
        sent_ack(&out[0]),
        (AckType::Reject, "1".into(), Some("expected HEL.R01".into()))
    );
    assert_eq!(out.len(), 1);

    ready(&mut host, base, 0);
    host.handle_message(device("XYZ.R01", "2", vec![]), at(base, 3))
        .unwrap();
    let out = outputs(&mut host);
    assert_eq!(
        sent_ack(&out[0]),
        (
            AckType::Reject,
            "2".into(),
            Some("unsupported message type".into())
        )
    );

    host.handle_message(device("REQ.R01", "3", vec![]), at(base, 4))
        .unwrap();
    assert_eq!(sent_ack(&outputs(&mut host)[0]).0, AckType::Reject);

    let anonymous = Message::from_element(Element::new("KPA.R01")).unwrap();
    host.handle_message(anonymous, at(base, 5)).unwrap();
    assert_eq!(
        sent_ack(&outputs(&mut host)[0]),
        (
            AckType::Reject,
            String::new(),
            Some("missing HDR.control_id".into())
        )
    );

    // Acknowledgments that match nothing are ignored.
    host.handle_message(device_ack("4", "AA", "999"), at(base, 6))
        .unwrap();
    assert!(outputs(&mut host).is_empty());
    assert_eq!(host.state(), ConversationState::Ready);
}

#[test]
fn closes_when_the_device_does_not_answer() {
    let base = Instant::now();
    let mut host = HostConversation::new(HostConfig::default(), base).unwrap();
    ready(&mut host, base, 1);
    assert_eq!(host.poll_timeout(), Some(base + Duration::from_secs(32)));
    host.handle_timeout(at(base, 31)).unwrap();
    assert!(outputs(&mut host).is_empty());
    host.handle_timeout(at(base, 32)).unwrap();
    let out = outputs(&mut host);
    assert_eq!(sent(&out[0]).kind(), MessageKind::Terminate);
    assert_eq!(out[1], Output::Closed(CloseReason::ResponseTimeout));
}

#[test]
fn device_can_end_the_conversation() {
    let base = Instant::now();
    let mut host = HostConversation::new(HostConfig::default(), base).unwrap();
    ready(&mut host, base, 0);
    host.handle_message(device("END.R01", "9", vec![]), at(base, 3))
        .unwrap();
    let out = outputs(&mut host);
    assert_eq!(sent_ack(&out[0]), (AckType::Accept, "9".into(), None));
    assert_eq!(out[1], Output::Closed(CloseReason::DeviceTerminated));
    assert_eq!(host.terminate(at(base, 4)), Err(ConversationError::Closed));
}

#[test]
fn keeps_alive_and_closes_idle_conversations() {
    let base = Instant::now();
    let mut config = HostConfig::default();
    config.keep_alive_interval = Some(Duration::from_secs(10));
    config.idle_timeout = Some(Duration::from_secs(60));
    let mut host = HostConversation::new(config, base).unwrap();
    ready(&mut host, base, 0);
    // The last host message (the status acknowledgment) was sent at 2 s.
    assert_eq!(host.poll_timeout(), Some(base + Duration::from_secs(12)));
    host.handle_timeout(at(base, 12)).unwrap();
    let out = outputs(&mut host);
    let keep_alive = sent(&out[0]);
    assert_eq!(keep_alive.kind(), MessageKind::KeepAlive);
    let id = keep_alive.control_id().unwrap().to_owned();
    host.handle_message(device_ack("50", "AA", &id), at(base, 13))
        .unwrap();
    assert_eq!(outputs(&mut host), [Output::Accepted { control_id: id }]);

    let mut config = HostConfig::default();
    config.idle_timeout = Some(Duration::from_secs(60));
    let mut host = HostConversation::new(config, base).unwrap();
    ready(&mut host, base, 0);
    assert_eq!(host.poll_timeout(), Some(base + Duration::from_secs(62)));
    host.handle_timeout(at(base, 62)).unwrap();
    let out = outputs(&mut host);
    assert_eq!(sent(&out[0]).kind(), MessageKind::Terminate);
    assert_eq!(out[1], Output::Closed(CloseReason::IdleTimeout));
}

#[test]
fn reports_rejected_host_messages() {
    let base = Instant::now();
    let mut host = HostConversation::new(HostConfig::default(), base).unwrap();
    ready(&mut host, base, 0);
    host.send_directive(directive::START_CONTINUOUS, &[], at(base, 3))
        .unwrap();
    let out = outputs(&mut host);
    let dtv = sent(&out[0]);
    assert_eq!(dtv.value("DTV/DTV.command_cd"), Some("START_CONTINUOUS"));
    let id = dtv.control_id().unwrap().to_owned();
    assert_eq!(
        host.request_observations(at(base, 3)),
        Err(ConversationError::Busy)
    );
    let reject = device(
        "ACK.R01",
        "60",
        vec![
            Element::new("ACK")
                .child(Element::with_v("ACK.type_cd", "AR"))
                .child(Element::with_v("ACK.ack_control_id", &id))
                .child(Element::with_v("ACK.note_txt", "not supported")),
        ],
    );
    host.handle_message(reject, at(base, 4)).unwrap();
    assert_eq!(
        outputs(&mut host),
        [Output::Rejected {
            control_id: id,
            ack_type: AckType::Reject,
            note: Some("not supported".into())
        }]
    );
    assert_eq!(host.state(), ConversationState::Ready);
    host.request_observations(at(base, 5)).unwrap();
}

#[test]
fn acknowledges_automatically_when_configured() {
    let base = Instant::now();
    let mut config = HostConfig::default();
    config.data_acknowledgment = DataAcknowledgment::Automatic;
    let mut host = HostConversation::new(config, base).unwrap();
    ready(&mut host, base, 0);
    let events = device("EVS.R01", "70", vec![Element::new("EVS")]);
    host.handle_message(events.clone(), at(base, 3)).unwrap();
    let out = outputs(&mut host);
    assert_eq!(out[0], Output::Events(events));
    assert_eq!(sent_ack(&out[1]), (AckType::Accept, "70".into(), None));
    assert!(host.awaiting_acknowledgment().is_empty());
}

#[test]
fn validates_actions_and_inputs() {
    let base = Instant::now();
    let mut host = HostConversation::new(HostConfig::default(), base).unwrap();
    assert_eq!(
        host.request_observations(at(base, 1)),
        Err(ConversationError::NotReady)
    );
    let bad_time = Now {
        instant: base,
        datetime: "\u{1}",
    };
    assert!(matches!(
        host.handle_message(hello("1"), bad_time),
        Err(ConversationError::Build(
            BuildError::InvalidCharacter { .. }
        ))
    ));
    assert_eq!(host.state(), ConversationState::AwaitingHello);
    assert!(outputs(&mut host).is_empty());

    let mut config = HostConfig::default();
    config.version_id = "\u{0}".into();
    assert!(HostConversation::new(config, base).is_err());
}
