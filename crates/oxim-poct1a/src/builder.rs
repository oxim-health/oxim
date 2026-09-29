//! Builders for the messages a host (data manager) sends.
//!
//! Every message starts with a header (`HDR`) holding a control ID, the
//! protocol version and the creation time. The caller supplies all three so
//! this crate never reads a clock.

use crate::content::AckType;
use crate::element::Element;
use crate::error::BuildError;
use crate::message::{Message, WriteOptions};

/// The usual `HDR.version_id` value.
pub const VERSION_ID: &str = "POCT1";

/// `REQ.request_cd` asking the device to send its observations, as used by
/// common implementations. Confirm the code with the device's interface
/// specification.
pub const REQUEST_OBSERVATIONS: &str = "ROBS";

/// Directive commands (`DTV.command_cd`) found in published device
/// interface documents. Devices differ; confirm the supported commands with
/// the device's interface specification.
pub mod directive {
    /// Ask the device to send new observations as soon as they are made.
    pub const START_CONTINUOUS: &str = "START_CONTINUOUS";
    /// Stop continuous sending.
    pub const STOP_CONTINUOUS: &str = "STOP_CONTINUOUS";
    /// Lock the device against use.
    pub const LOCK: &str = "LOCK";
    /// Unlock the device.
    pub const UNLOCK: &str = "UNLOCK";
}

/// The header (`HDR`) of an outgoing message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Header<'a> {
    /// `HDR.control_id`, unique per message within the conversation.
    pub control_id: &'a str,
    /// `HDR.version_id`, usually [`VERSION_ID`].
    pub version_id: &'a str,
    /// `HDR.creation_dttm`, an ISO 8601 date and time with offset, for
    /// example `2026-09-29T12:00:00+03:00`.
    pub creation_dttm: &'a str,
}

impl Header<'_> {
    /// The `HDR` element.
    pub fn to_element(&self) -> Element {
        Element::new("HDR")
            .child(Element::with_v("HDR.control_id", self.control_id))
            .child(Element::with_v("HDR.version_id", self.version_id))
            .child(Element::with_v("HDR.creation_dttm", self.creation_dttm))
    }
}

/// Builds `<message_type><HDR/>body...</message_type>`.
pub fn build(
    message_type: &str,
    header: &Header<'_>,
    body: impl IntoIterator<Item = Element>,
    options: &WriteOptions,
) -> Result<Message, BuildError> {
    let mut root = Element::new(message_type).child(header.to_element());
    for element in body {
        root.push_child(element);
    }
    Message::from_element_with(root, options)
}

/// `ACK.R01` acknowledging the message with control ID `acked_control_id`.
pub fn ack(
    header: &Header<'_>,
    ack_type: AckType,
    acked_control_id: &str,
    note: Option<&str>,
) -> Result<Message, BuildError> {
    let mut body = Element::new("ACK")
        .child(Element::with_v("ACK.type_cd", ack_type.as_str()))
        .child(Element::with_v("ACK.ack_control_id", acked_control_id));
    if let Some(note) = note {
        body.push_child(Element::with_v("ACK.note_txt", note));
    }
    build("ACK.R01", header, [body], &WriteOptions::default())
}

/// `REQ.R01` with `REQ.request_cd`, for example [`REQUEST_OBSERVATIONS`].
pub fn request(header: &Header<'_>, request_cd: &str) -> Result<Message, BuildError> {
    let body = Element::new("REQ").child(Element::with_v("REQ.request_cd", request_cd));
    build("REQ.R01", header, [body], &WriteOptions::default())
}

/// `EOT.R01` ending the topic `topic_cd`, for example `OBS` or `OPL`.
pub fn end_of_topic(header: &Header<'_>, topic_cd: &str) -> Result<Message, BuildError> {
    let body = Element::new("EOT").child(Element::with_v("EOT.topic_cd", topic_cd));
    build("EOT.R01", header, [body], &WriteOptions::default())
}

/// `END.R01` ending the conversation, with an optional `END.reason_cd`.
pub fn terminate(header: &Header<'_>, reason_cd: Option<&str>) -> Result<Message, BuildError> {
    let body =
        reason_cd.map(|reason| Element::new("END").child(Element::with_v("END.reason_cd", reason)));
    build("END.R01", header, body, &WriteOptions::default())
}

/// A directive with `DTV.command_cd`. Without parameters the message is
/// `DTV.R01`; with parameters it is `DTV.R02` and each `(name, value)` pair
/// becomes `<DTV.name V="value"/>`.
pub fn directive(
    header: &Header<'_>,
    command_cd: &str,
    parameters: &[(&str, &str)],
) -> Result<Message, BuildError> {
    let mut body = Element::new("DTV").child(Element::with_v("DTV.command_cd", command_cd));
    for (name, value) in parameters {
        body.push_child(Element::with_v(format!("DTV.{name}"), *value));
    }
    let message_type = if parameters.is_empty() {
        "DTV.R01"
    } else {
        "DTV.R02"
    };
    build(message_type, header, [body], &WriteOptions::default())
}

/// `KPA.R01`, a keep-alive message that carries only a header.
pub fn keep_alive(header: &Header<'_>) -> Result<Message, BuildError> {
    build("KPA.R01", header, [], &WriteOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageKind;

    const HEADER: Header<'static> = Header {
        control_id: "42",
        version_id: VERSION_ID,
        creation_dttm: "2026-09-29T12:00:00+03:00",
    };

    #[test]
    fn builds_acknowledgments() {
        let message = ack(&HEADER, AckType::Error, "7", Some("unknown test")).unwrap();
        assert_eq!(
            message.as_bytes(),
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?><ACK.R01><HDR><HDR.control_id V="42"/>"#,
                r#"<HDR.version_id V="POCT1"/><HDR.creation_dttm V="2026-09-29T12:00:00+03:00"/></HDR>"#,
                r#"<ACK><ACK.type_cd V="AE"/><ACK.ack_control_id V="7"/><ACK.note_txt V="unknown test"/></ACK></ACK.R01>"#
            )
            .as_bytes()
        );
        let parsed = Message::parse(message.as_bytes()).unwrap();
        let info = parsed.ack_info().unwrap();
        assert_eq!(info.ack_type, Some(AckType::Error));
        assert_eq!(info.acked_control_id.as_deref(), Some("7"));
        assert_eq!(parsed.control_id(), Some("42"));
    }

    #[test]
    fn builds_host_messages() {
        let req = request(&HEADER, REQUEST_OBSERVATIONS).unwrap();
        assert_eq!(req.kind(), MessageKind::Request);
        assert_eq!(req.value("REQ/REQ.request_cd"), Some("ROBS"));

        let eot = end_of_topic(&HEADER, "OPL").unwrap();
        assert_eq!(eot.end_of_topic(), Some("OPL"));

        let end = terminate(&HEADER, None).unwrap();
        assert_eq!(end.message_type(), "END.R01");
        assert!(end.find("END").is_none());
        let end = terminate(&HEADER, Some("NRM")).unwrap();
        assert_eq!(end.value("END/END.reason_cd"), Some("NRM"));

        let lock = directive(&HEADER, directive::LOCK, &[]).unwrap();
        assert_eq!(lock.message_type(), "DTV.R01");
        let timed = directive(&HEADER, "SET_TIME", &[("dttm", "2026-09-29T12:00:00Z")]).unwrap();
        assert_eq!(timed.message_type(), "DTV.R02");
        assert_eq!(timed.value("DTV/DTV.dttm"), Some("2026-09-29T12:00:00Z"));

        assert_eq!(keep_alive(&HEADER).unwrap().kind(), MessageKind::KeepAlive);
    }

    #[test]
    fn rejects_invalid_content() {
        let bad = Header {
            control_id: "\u{0}",
            ..HEADER
        };
        assert!(matches!(
            ack(&bad, AckType::Accept, "1", None),
            Err(BuildError::InvalidCharacter { .. })
        ));
        assert!(matches!(
            directive(&HEADER, "X", &[("bad name", "1")]),
            Err(BuildError::InvalidName(_))
        ));
    }
}
