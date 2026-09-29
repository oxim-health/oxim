//! Small XML helpers over quick-xml: element texts by path, and the raw
//! content of an element.

use quick_xml::events::{BytesRef, Event};

/// The character an entity or character reference stands for.
fn resolve(reference: &BytesRef<'_>) -> Option<char> {
    match reference.resolve_char_ref() {
        Ok(Some(c)) => Some(c),
        Ok(None) => match &**reference {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            "quot" => Some('"'),
            _ => None,
        },
        Err(_) => None,
    }
}

/// Calls `visit` with the path of local element names and the text of
/// every element when it ends. Element texts include only text directly
/// inside the element.
pub(crate) fn walk(input: &[u8], mut visit: impl FnMut(&[String], &str)) -> Result<(), String> {
    let mut reader = quick_xml::Reader::from_reader(input);
    let mut path: Vec<String> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                path.push(start.local_name().as_ref().to_owned());
                texts.push(String::new());
            }
            Ok(Event::Empty(empty)) => {
                path.push(empty.local_name().as_ref().to_owned());
                visit(&path, "");
                path.pop();
            }
            Ok(Event::Text(text)) => {
                if let Some(current) = texts.last_mut() {
                    current.push_str(&text.xml10_content());
                }
            }
            Ok(Event::CData(data)) => {
                if let Some(current) = texts.last_mut() {
                    current.push_str(&data.xml10_content());
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if let (Some(current), Some(c)) = (texts.last_mut(), resolve(&reference)) {
                    current.push(c);
                }
            }
            Ok(Event::End(_)) => {
                let text = texts.pop().unwrap_or_default();
                visit(&path, &text);
                path.pop();
            }
            Ok(Event::DocType(_)) => {
                return Err("document type declarations are not accepted".into());
            }
            Ok(Event::Eof) => return Ok(()),
            Ok(_) => {}
            Err(e) => {
                return Err(format!(
                    "invalid XML at byte {}: {e}",
                    reader.buffer_position()
                ));
            }
        }
    }
}

/// The byte range of the content of the first element whose path of local
/// names ends with `suffix` (for example `["Envelope", "Body"]`), and the
/// namespace-qualified name of the root element.
pub(crate) fn content_range(
    input: &[u8],
    suffix: &[&str],
) -> Result<Option<(usize, usize)>, String> {
    let mut reader = quick_xml::Reader::from_reader(input);
    let mut path: Vec<String> = Vec::new();
    let mut start_at: Option<(usize, usize)> = None;
    loop {
        let before = reader.buffer_position();
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                path.push(start.local_name().as_ref().to_owned());
                if start_at.is_none() && path.ends_with_names(suffix) {
                    start_at = Some((
                        usize::try_from(reader.buffer_position()).unwrap_or(0),
                        path.len(),
                    ));
                }
            }
            Ok(Event::Empty(empty)) => {
                path.push(empty.local_name().as_ref().to_owned());
                if start_at.is_none() && path.ends_with_names(suffix) {
                    let at = usize::try_from(reader.buffer_position()).unwrap_or(0);
                    return Ok(Some((at, at)));
                }
                path.pop();
            }
            Ok(Event::End(_)) => {
                if let Some((from, depth)) = start_at
                    && depth == path.len()
                {
                    return Ok(Some((from, usize::try_from(before).unwrap_or(from))));
                }
                path.pop();
            }
            Ok(Event::DocType(_)) => {
                return Err("document type declarations are not accepted".into());
            }
            Ok(Event::Eof) => return Ok(None),
            Ok(_) => {}
            Err(e) => {
                return Err(format!(
                    "invalid XML at byte {}: {e}",
                    reader.buffer_position()
                ));
            }
        }
    }
}

trait EndsWithNames {
    fn ends_with_names(&self, suffix: &[&str]) -> bool;
}

impl EndsWithNames for Vec<String> {
    fn ends_with_names(&self, suffix: &[&str]) -> bool {
        self.len() >= suffix.len()
            && self[self.len() - suffix.len()..]
                .iter()
                .zip(suffix)
                .all(|(a, b)| a == b)
    }
}

/// The namespace URI of the root element's prefix, looked up in its
/// `xmlns` declarations.
pub(crate) fn root_namespace(input: &[u8]) -> Option<String> {
    let mut reader = quick_xml::Reader::from_reader(input);
    loop {
        match reader.read_event() {
            Ok(Event::Start(start) | Event::Empty(start)) => {
                let name = start.name();
                let qualified: &str = name.as_ref();
                let prefix = qualified.split_once(':').map(|(prefix, _)| prefix);
                let wanted = match prefix {
                    Some(prefix) => format!("xmlns:{prefix}"),
                    None => "xmlns".to_owned(),
                };
                for attribute in start.attributes().flatten() {
                    let key: &str = attribute.key.as_ref();
                    if key == wanted {
                        return attribute
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .ok()
                            .map(|value| value.into_owned());
                    }
                }
                return None;
            }
            Ok(Event::Eof) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_texts_with_references() {
        let mut seen = Vec::new();
        walk(b"<a><b>x&amp;y&#65;</b><c/></a>", |path, text| {
            seen.push((path.join("/"), text.to_owned()));
        })
        .unwrap();
        assert_eq!(
            seen,
            [
                ("a/b".to_owned(), "x&yA".to_owned()),
                ("a/c".to_owned(), String::new()),
                ("a".to_owned(), String::new())
            ]
        );
    }

    #[test]
    fn finds_element_content() {
        let input = br#"<s:Envelope xmlns:s="urn:x"><s:Header/><s:Body><m:Order xmlns:m="urn:m">1</m:Order></s:Body></s:Envelope>"#;
        let (from, to) = content_range(input, &["Envelope", "Body"])
            .unwrap()
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&input[from..to]).unwrap(),
            r#"<m:Order xmlns:m="urn:m">1</m:Order>"#
        );
        assert_eq!(root_namespace(input).as_deref(), Some("urn:x"));
        assert!(content_range(b"<a/>", &["Body"]).unwrap().is_none());
    }
}
