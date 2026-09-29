//! POCT1-A messages: parsing, accessors and serialization.

use std::borrow::Cow;

use encoding_rs::{Encoding, UTF_8};
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::element::{Element, is_xml_char};
use crate::error::{BuildError, ParseError};

/// Limits applied by [`Message::parse_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseOptions {
    /// The largest document accepted, in bytes.
    pub max_len: usize,
    /// The deepest element nesting accepted.
    pub max_depth: usize,
    /// The largest number of elements accepted.
    pub max_elements: usize,
    /// The largest number of attributes accepted on one element.
    pub max_attributes: usize,
    /// The largest total amount of character data accepted, in bytes.
    pub max_text_len: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            max_len: 4 * 1024 * 1024,
            max_depth: 32,
            max_elements: 100_000,
            max_attributes: 64,
            max_text_len: 1024 * 1024,
        }
    }
}

/// Whether a message is written with an XML declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteOptions {
    /// Write `<?xml version="1.0" encoding="UTF-8"?>` before the root element.
    pub declaration: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self { declaration: true }
    }
}

impl WriteOptions {
    /// Options that write (`true`) or omit (`false`) the XML declaration.
    pub const fn with_declaration(declaration: bool) -> Self {
        Self { declaration }
    }
}

/// The POCT1-A message family, taken from the root element name
/// (`OBS.R01` belongs to [`MessageKind::Observation`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MessageKind {
    /// `HEL`: the device introduces itself.
    Hello,
    /// `DST`: device status.
    DeviceStatus,
    /// `OBS`: observations (patient, quality control, calibration).
    Observation,
    /// `OPL`: operator list.
    OperatorList,
    /// `DTV`: directive from the host.
    Directive,
    /// `EVS`: device events.
    Event,
    /// `REQ`: request from the host.
    Request,
    /// `EOT`: end of topic.
    EndOfTopic,
    /// `ACK`: acknowledgment.
    Acknowledgment,
    /// `END`: end of conversation.
    Terminate,
    /// `KPA`: keep-alive.
    KeepAlive,
    /// Any other message type.
    Other,
}

impl MessageKind {
    /// The kind for a topic code such as `OBS`.
    pub fn from_topic(topic: &str) -> Self {
        match topic {
            "HEL" => Self::Hello,
            "DST" => Self::DeviceStatus,
            "OBS" => Self::Observation,
            "OPL" => Self::OperatorList,
            "DTV" => Self::Directive,
            "EVS" => Self::Event,
            "REQ" => Self::Request,
            "EOT" => Self::EndOfTopic,
            "ACK" => Self::Acknowledgment,
            "END" => Self::Terminate,
            "KPA" => Self::KeepAlive,
            _ => Self::Other,
        }
    }
}

/// A POCT1-A message.
///
/// A parsed message keeps its original bytes, so [`Message::as_bytes`]
/// returns exactly what was received, together with the element tree used
/// by the accessors. A message built from an [`Element`] holds the bytes of
/// its serialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message {
    raw: Vec<u8>,
    root: Element,
}

impl Message {
    /// Parses one XML document with the default [`ParseOptions`].
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with(bytes, &ParseOptions::default())
    }

    /// Parses one XML document.
    ///
    /// UTF-8 (with or without a byte order mark) and ASCII-compatible
    /// encodings declared in the XML declaration, such as ISO-8859-1, are
    /// accepted. Document type declarations are rejected, so no external or
    /// internal entity can be defined; only the five predefined entities and
    /// character references are resolved.
    pub fn parse_with(bytes: &[u8], options: &ParseOptions) -> Result<Self, ParseError> {
        if bytes.len() > options.max_len {
            return Err(ParseError::LimitExceeded("document length"));
        }
        let text = decode(bytes)?;
        let root = build_tree(&text, options)?;
        // The XML reader does not check every well-formedness rule for names
        // and characters; enforce them so every parsed message can also be
        // written back.
        root.validate().map_err(ParseError::InvalidContent)?;
        Ok(Self {
            raw: bytes.to_vec(),
            root,
        })
    }

    /// Serializes `root` with the default [`WriteOptions`].
    pub fn from_element(root: Element) -> Result<Self, BuildError> {
        Self::from_element_with(root, &WriteOptions::default())
    }

    /// Serializes `root` after validating names and characters.
    pub fn from_element_with(root: Element, options: &WriteOptions) -> Result<Self, BuildError> {
        root.validate()?;
        let mut out = String::new();
        if options.declaration {
            out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        }
        root.write(&mut out);
        Ok(Self {
            raw: out.into_bytes(),
            root,
        })
    }

    /// The message bytes: the original input of a parsed message.
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// The message bytes as an owned vector.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.raw.clone()
    }

    /// Consumes the message and returns its bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.raw
    }

    /// The root element.
    pub fn root(&self) -> &Element {
        &self.root
    }

    /// The message type: the root element name, for example `OBS.R01`.
    pub fn message_type(&self) -> &str {
        self.root.name()
    }

    /// The topic part of the message type, for example `OBS`.
    pub fn topic(&self) -> &str {
        self.root
            .name()
            .split_once('.')
            .map_or(self.root.name(), |(topic, _)| topic)
    }

    /// The trigger part of the message type, for example `R01`.
    pub fn trigger(&self) -> Option<&str> {
        self.root.name().split_once('.').map(|(_, trigger)| trigger)
    }

    /// The message family.
    pub fn kind(&self) -> MessageKind {
        MessageKind::from_topic(self.topic())
    }

    /// `HDR.control_id`, which acknowledgments echo.
    pub fn control_id(&self) -> Option<&str> {
        self.value("HDR/HDR.control_id")
    }

    /// `HDR.version_id`, usually `POCT1`.
    pub fn version_id(&self) -> Option<&str> {
        self.value("HDR/HDR.version_id")
    }

    /// `HDR.creation_dttm`, an ISO 8601 date and time.
    pub fn creation_dttm(&self) -> Option<&str> {
        self.value("HDR/HDR.creation_dttm")
    }

    /// The first element matching a path relative to the root, such as
    /// `SVC/PT/OBS`. A leading segment equal to the message type is
    /// accepted and skipped (`OBS.R01/SVC/PT`).
    pub fn find(&self, path: &str) -> Option<&Element> {
        self.root.find(self.relative(path))
    }

    /// All elements matching a path relative to the root.
    pub fn find_all(&self, path: &str) -> Vec<&Element> {
        self.root.find_all(self.relative(path))
    }

    /// The `V` attribute of the first element matching a path relative to
    /// the root, for example `SVC/PT/OBS/OBS.observation_id`.
    pub fn value(&self, path: &str) -> Option<&str> {
        self.root.value_at(self.relative(path))
    }

    fn relative<'p>(&self, path: &'p str) -> &'p str {
        path.strip_prefix(self.root.name())
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(path)
    }
}

/// Decodes the document to UTF-8 text.
fn decode(bytes: &[u8]) -> Result<Cow<'_, str>, ParseError> {
    if let Some((encoding, _)) = Encoding::for_bom(bytes)
        && encoding != UTF_8
    {
        return Err(ParseError::UnsupportedEncoding(encoding.name().to_owned()));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let encoding = match declared_encoding(bytes) {
        None => UTF_8,
        Some(label) => {
            let encoding = Encoding::for_label(label.trim_ascii())
                .ok_or_else(|| ParseError::UnsupportedEncoding(lossy(label)))?;
            let ascii_compatible = encoding.is_ascii_compatible()
                && encoding != encoding_rs::ISO_2022_JP
                && encoding != encoding_rs::REPLACEMENT;
            if !ascii_compatible {
                return Err(ParseError::UnsupportedEncoding(lossy(label)));
            }
            encoding
        }
    };
    encoding
        .decode_without_bom_handling_and_without_replacement(bytes)
        .ok_or(ParseError::InvalidEncoding(encoding.name()))
}

/// The `encoding` pseudo-attribute of the XML declaration, if any.
fn declared_encoding(bytes: &[u8]) -> Option<&[u8]> {
    let rest = bytes.strip_prefix(b"<?xml")?;
    if !rest.first().is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    let end = memchr::memmem::find(rest, b"?>")?;
    let declaration = &rest[..end];
    let at = memchr::memmem::find(declaration, b"encoding")?;
    let after = declaration[at + b"encoding".len()..].trim_ascii_start();
    let after = after.strip_prefix(b"=")?.trim_ascii_start();
    let quote = *after.first().filter(|q| matches!(q, b'"' | b'\''))?;
    let value = &after[1..];
    let close = memchr::memchr(quote, value)?;
    Some(&value[..close])
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn syntax(reader: &Reader<&[u8]>, error: impl std::fmt::Display) -> ParseError {
    ParseError::Syntax {
        position: reader.error_position(),
        message: error.to_string(),
    }
}

/// Builds the element tree, enforcing the limits and security rules.
fn build_tree(text: &str, options: &ParseOptions) -> Result<Element, ParseError> {
    let mut reader = Reader::from_str(text);
    let config = reader.config_mut();
    config.check_end_names = true;
    config.expand_empty_elements = false;
    config.trim_text(false);

    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut elements = 0usize;
    let mut text_len = 0usize;

    loop {
        let event = reader.read_event().map_err(|e| syntax(&reader, e))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if root.is_some() {
                    return Err(ParseError::MultipleRootElements);
                }
                elements += 1;
                if elements > options.max_elements {
                    return Err(ParseError::LimitExceeded("elements"));
                }
                if stack.len() >= options.max_depth {
                    return Err(ParseError::LimitExceeded("nesting depth"));
                }
                let mut element = Element::new(start.name().as_ref());
                for (count, attribute) in start.attributes().enumerate() {
                    if count >= options.max_attributes {
                        return Err(ParseError::LimitExceeded("attributes per element"));
                    }
                    let attribute = attribute.map_err(|e| syntax(&reader, e))?;
                    let value = attribute
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|e| entity_error(&reader, e))?;
                    text_len += value.len();
                    if text_len > options.max_text_len {
                        return Err(ParseError::LimitExceeded("text length"));
                    }
                    element.set_attribute(attribute.key.as_ref(), value.into_owned());
                }
                if matches!(event, Event::Empty(_)) {
                    close(element, &mut stack, &mut root);
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => match stack.pop() {
                Some(element) => close(element, &mut stack, &mut root),
                None => return Err(syntax(&reader, "unexpected closing tag")),
            },
            Event::Text(ref content) => {
                let content = content.xml10_content();
                append_text(&mut stack, &content, &mut text_len, options)?;
            }
            Event::CData(ref content) => {
                let content = content.xml10_content();
                append_text(&mut stack, &content, &mut text_len, options)?;
            }
            Event::GeneralRef(ref reference) => {
                let resolved = match reference.resolve_char_ref() {
                    Ok(Some(c)) if is_xml_char(c) => c,
                    Ok(Some(_)) | Err(_) => {
                        return Err(syntax(&reader, "invalid character reference"));
                    }
                    Ok(None) => match &**reference {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "apos" => '\'',
                        "quot" => '"',
                        name => return Err(ParseError::UnknownEntity(name.to_owned())),
                    },
                };
                append_text(
                    &mut stack,
                    resolved.encode_utf8(&mut [0; 4]),
                    &mut text_len,
                    options,
                )?;
            }
            Event::DocType(_) => return Err(ParseError::Forbidden("document type declaration")),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if let Some(open) = stack.pop() {
        return Err(ParseError::UnclosedElement(open.name().to_owned()));
    }
    root.ok_or(ParseError::NoRootElement)
}

fn close(element: Element, stack: &mut [Element], root: &mut Option<Element>) {
    match stack.last_mut() {
        Some(parent) => parent.push_child(element),
        None => *root = Some(element),
    }
}

fn append_text(
    stack: &mut [Element],
    text: &str,
    total: &mut usize,
    options: &ParseOptions,
) -> Result<(), ParseError> {
    match stack.last_mut() {
        Some(element) => {
            *total += text.len();
            if *total > options.max_text_len {
                return Err(ParseError::LimitExceeded("text length"));
            }
            element.push_text(text);
            Ok(())
        }
        None if text.bytes().all(|b| b.is_ascii_whitespace()) => Ok(()),
        None => Err(ParseError::TextOutsideRoot),
    }
}

/// Maps an attribute unescaping error, reporting unknown entities by name.
fn entity_error(reader: &Reader<&[u8]>, error: quick_xml::Error) -> ParseError {
    use quick_xml::escape::EscapeError;
    match &error {
        quick_xml::Error::Escape(EscapeError::UnrecognizedEntity(_, name)) => {
            ParseError::UnknownEntity(name.clone())
        }
        _ => syntax(reader, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<OBS.R01>
  <HDR>
    <HDR.control_id V="1002"/>
    <HDR.version_id V="POCT1"/>
    <HDR.creation_dttm V="2026-09-29T12:00:00+03:00"/>
  </HDR>
  <!-- one patient result -->
  <SVC>
    <SVC.role_cd V="OBS"/>
    <SVC.observation_dttm V="2026-09-29T11:58:00+03:00"/>
    <PT>
      <PT.patient_id V="P-001"/>
      <OBS>
        <OBS.observation_id V="GLU" SN="LOCAL"/>
        <OBS.value V="5.4" U="mmol/L"/>
      </OBS>
    </PT>
    <NTE><NTE.text V="a &amp; b &#x3C; c"/>Free &lt;text&gt;</NTE>
  </SVC>
</OBS.R01>
"#;

    #[test]
    fn parses_and_keeps_original_bytes() {
        let message = Message::parse(OBS.as_bytes()).unwrap();
        assert_eq!(message.as_bytes(), OBS.as_bytes());
        assert_eq!(message.message_type(), "OBS.R01");
        assert_eq!(message.topic(), "OBS");
        assert_eq!(message.trigger(), Some("R01"));
        assert_eq!(message.kind(), MessageKind::Observation);
        assert_eq!(message.control_id(), Some("1002"));
        assert_eq!(message.version_id(), Some("POCT1"));
        assert_eq!(message.creation_dttm(), Some("2026-09-29T12:00:00+03:00"));
        assert_eq!(message.value("SVC/PT/OBS/OBS.observation_id"), Some("GLU"));
        assert_eq!(message.value("OBS.R01/SVC/PT/PT.patient_id"), Some("P-001"));
        assert_eq!(
            message
                .find("SVC/PT/OBS/OBS.value")
                .unwrap()
                .attribute_value("U"),
            Some("mmol/L")
        );
        let note = message.find("SVC/NTE").unwrap();
        assert_eq!(note.value_at("NTE.text"), Some("a & b < c"));
        assert_eq!(note.text_content(), "Free <text>");
    }

    #[test]
    fn built_messages_round_trip() {
        let root = Element::new("ACK.R01")
            .child(Element::new("HDR").child(Element::with_v("HDR.control_id", "1")))
            .child(
                Element::new("ACK").child(Element::with_v("ACK.note_txt", "a\tb\nc\rd \"q\" <&>")),
            )
            .child(Element::new("NTE").text("line 1\r\nline 2 ]]> & <"));
        let message = Message::from_element(root.clone()).unwrap();
        let parsed = Message::parse(message.as_bytes()).unwrap();
        assert_eq!(parsed.root(), &root);
        assert_eq!(parsed, message);

        let bare = Message::from_element_with(
            Element::new("KPA.R01"),
            &WriteOptions { declaration: false },
        )
        .unwrap();
        assert_eq!(bare.as_bytes(), b"<KPA.R01/>");
    }

    #[test]
    fn decodes_declared_single_byte_encodings() {
        let mut bytes = b"<?xml version='1.0' encoding='ISO-8859-9'?><NTE V='\xDEahin'/>".to_vec();
        let message = Message::parse(&bytes).unwrap();
        assert_eq!(message.root().value(), Some("Şahin"));
        assert_eq!(message.as_bytes(), bytes.as_slice());

        bytes.insert(0, 0);
        assert!(Message::parse(&bytes).is_err());
        let with_bom = b"\xEF\xBB\xBF<A V='\xC3\xA7'/>";
        assert_eq!(Message::parse(with_bom).unwrap().root().value(), Some("ç"));
    }

    #[test]
    fn rejects_unsupported_encodings() {
        assert_eq!(
            Message::parse(b"<?xml version='1.0' encoding='UTF-16'?><A/>"),
            Err(ParseError::UnsupportedEncoding("UTF-16".into()))
        );
        assert!(matches!(
            Message::parse(b"\xFF\xFE<\0A\0/\0>\0"),
            Err(ParseError::UnsupportedEncoding(_))
        ));
        assert_eq!(
            Message::parse(b"<A V='\xFF'/>"),
            Err(ParseError::InvalidEncoding("UTF-8"))
        );
    }

    #[test]
    fn rejects_dangerous_or_malformed_documents() {
        let cases: &[(&[u8], ParseError)] = &[
            (
                b"<?xml version='1.0'?><!DOCTYPE a [<!ENTITY x 'boom'>]><a>&x;</a>",
                ParseError::Forbidden("document type declaration"),
            ),
            (b"<a>&x;</a>", ParseError::UnknownEntity("x".into())),
            (b"<a V='&x;'/>", ParseError::UnknownEntity("x".into())),
            (b"", ParseError::NoRootElement),
            (b"  <!-- only a comment -->  ", ParseError::NoRootElement),
            (b"<a/><b/>", ParseError::MultipleRootElements),
            (b"<a><b/>", ParseError::UnclosedElement("a".into())),
            (b"junk<a/>", ParseError::TextOutsideRoot),
            (b"<a/>junk", ParseError::TextOutsideRoot),
            (
                b"<a V='\x01'/>",
                ParseError::InvalidContent(BuildError::InvalidCharacter {
                    context: "attribute V of <a>".into(),
                    code: 1,
                }),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                Message::parse(input).as_ref(),
                Err(expected),
                "{}",
                String::from_utf8_lossy(input)
            );
        }
        for input in [
            &b"<a></b>"[..],
            b"<a V='1' V='2'/>",
            b"<a V=1/>",
            b"<a>&#0;</a>",
            b"<",
        ] {
            assert!(
                matches!(Message::parse(input), Err(ParseError::Syntax { .. })),
                "{}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn enforces_limits() {
        let deep = format!("{}{}", "<a>".repeat(40), "</a>".repeat(40));
        assert_eq!(
            Message::parse(deep.as_bytes()),
            Err(ParseError::LimitExceeded("nesting depth"))
        );
        let options = ParseOptions {
            max_elements: 3,
            ..ParseOptions::default()
        };
        assert_eq!(
            Message::parse_with(b"<a><b/><c/><d/></a>", &options),
            Err(ParseError::LimitExceeded("elements"))
        );
        let options = ParseOptions {
            max_text_len: 4,
            ..ParseOptions::default()
        };
        assert_eq!(
            Message::parse_with(b"<a>hello</a>", &options),
            Err(ParseError::LimitExceeded("text length"))
        );
        let options = ParseOptions {
            max_len: 3,
            ..ParseOptions::default()
        };
        assert_eq!(
            Message::parse_with(b"<a/>", &options),
            Err(ParseError::LimitExceeded("document length"))
        );
        let options = ParseOptions {
            max_attributes: 1,
            ..ParseOptions::default()
        };
        assert_eq!(
            Message::parse_with(b"<a x='1' y='2'/>", &options),
            Err(ParseError::LimitExceeded("attributes per element"))
        );
    }

    #[test]
    fn classifies_message_kinds() {
        for (name, kind) in [
            ("HEL.R01", MessageKind::Hello),
            ("DST.R01", MessageKind::DeviceStatus),
            ("OBS.R02", MessageKind::Observation),
            ("OPL.R01", MessageKind::OperatorList),
            ("DTV.R02", MessageKind::Directive),
            ("EVS.R01", MessageKind::Event),
            ("REQ.R01", MessageKind::Request),
            ("EOT.R01", MessageKind::EndOfTopic),
            ("ACK.R01", MessageKind::Acknowledgment),
            ("END.R01", MessageKind::Terminate),
            ("KPA.R01", MessageKind::KeepAlive),
            ("XYZ.R01", MessageKind::Other),
        ] {
            let message = Message::from_element(Element::new(name)).unwrap();
            assert_eq!(message.kind(), kind, "{name}");
        }
    }
}
