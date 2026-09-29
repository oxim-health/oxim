//! A small read-only element tree of a Mirth Connect export. Every element
//! remembers its XPath-like location for the migration report.

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::error::MirthError;

/// Deepest accepted element nesting. Mirth exports stay far below it.
const MAX_DEPTH: usize = 256;

/// One XML element.
#[derive(Debug, Clone, Default)]
pub(crate) struct Element {
    pub(crate) name: String,
    attributes: Vec<(String, String)>,
    text: String,
    pub(crate) children: Vec<Element>,
    /// The location, for example `/channel/destinationConnectors/connector[2]`.
    pub(crate) location: String,
}

impl Element {
    /// The first child called `name`.
    pub(crate) fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    /// The children called `name`.
    pub(crate) fn children_named<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// The descendant at `path`, following the first matching child.
    pub(crate) fn find(&self, path: &[&str]) -> Option<&Element> {
        path.iter()
            .try_fold(self, |element, name| element.child(name))
    }

    /// The text of the element, untrimmed.
    pub(crate) fn raw_text(&self) -> &str {
        &self.text
    }

    /// The trimmed text of the child `name`, when not empty.
    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.child(name)
            .map(|child| child.text.trim())
            .filter(|text| !text.is_empty())
    }

    /// The untrimmed text of the child `name`.
    pub(crate) fn script(&self, name: &str) -> Option<&str> {
        self.child(name).map(|child| child.text.as_str())
    }

    /// The child `name` read as a boolean.
    pub(crate) fn flag(&self, name: &str) -> Option<bool> {
        match self.value(name)? {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// The child `name` read as a whole number.
    pub(crate) fn number(&self, name: &str) -> Option<u64> {
        self.value(name)?.parse().ok()
    }

    /// An attribute value.
    pub(crate) fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The trimmed text of each `string` child, as Mirth writes lists.
    pub(crate) fn strings(&self) -> Vec<String> {
        self.children_named("string")
            .map(|child| child.text.clone())
            .collect()
    }

    fn assign_locations(&mut self, parent: &str) {
        if self.location.is_empty() {
            self.location = format!("{parent}/{}", self.name);
        }
        let mut totals: Vec<(String, usize)> = Vec::new();
        for child in &self.children {
            match totals.iter_mut().find(|(name, _)| *name == child.name) {
                Some((_, count)) => *count += 1,
                None => totals.push((child.name.clone(), 1)),
            }
        }
        let mut seen: Vec<(String, usize)> = Vec::new();
        let location = self.location.clone();
        for child in &mut self.children {
            let index = match seen.iter_mut().find(|(name, _)| *name == child.name) {
                Some((_, count)) => {
                    *count += 1;
                    *count
                }
                None => {
                    seen.push((child.name.clone(), 1));
                    1
                }
            };
            let total = totals
                .iter()
                .find(|(name, _)| *name == child.name)
                .map_or(1, |(_, count)| *count);
            child.location = if total > 1 {
                format!("{location}/{}[{index}]", child.name)
            } else {
                format!("{location}/{}", child.name)
            };
            child.assign_locations(&location);
        }
    }
}

fn syntax(reader: &Reader<&[u8]>, message: impl std::fmt::Display) -> MirthError {
    MirthError::Xml(format!("at byte {}: {message}", reader.error_position()))
}

/// Parses an export. Document type declarations are rejected and only the
/// predefined entities and character references are resolved, so no
/// external content is ever read.
pub(crate) fn parse(text: &str) -> Result<Element, MirthError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader::from_str(text);
    let config = reader.config_mut();
    config.check_end_names = true;
    config.expand_empty_elements = false;
    config.trim_text(false);

    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        let event = reader.read_event().map_err(|e| syntax(&reader, e))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if root.is_some() {
                    return Err(syntax(&reader, "more than one root element"));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(syntax(&reader, "elements are nested too deeply"));
                }
                let name = AsRef::<str>::as_ref(&start.name()).to_owned();
                let mut element = Element {
                    name,
                    ..Element::default()
                };
                for attribute in start.attributes() {
                    let attribute = attribute.map_err(|e| syntax(&reader, e))?;
                    let value = attribute
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| syntax(&reader, e))?;
                    element.attributes.push((
                        AsRef::<str>::as_ref(&attribute.key).to_owned(),
                        value.into_owned(),
                    ));
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
                if let Some(element) = stack.last_mut() {
                    element.text.push_str(&content.xml10_content());
                }
            }
            Event::CData(ref content) => {
                if let Some(element) = stack.last_mut() {
                    element.text.push_str(&content.xml10_content());
                }
            }
            Event::GeneralRef(ref reference) => {
                let resolved = match reference.resolve_char_ref() {
                    Ok(Some(c)) => c,
                    Ok(None) => match &**reference {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "apos" => '\'',
                        "quot" => '"',
                        name => {
                            return Err(syntax(&reader, format!("unknown entity &{name};")));
                        }
                    },
                    Err(e) => return Err(syntax(&reader, e)),
                };
                if let Some(element) = stack.last_mut() {
                    element.text.push(resolved);
                }
            }
            Event::DocType(_) => {
                return Err(syntax(
                    &reader,
                    "document type declarations are not accepted",
                ));
            }
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if let Some(open) = stack.pop() {
        return Err(MirthError::Xml(format!(
            "element <{}> is not closed",
            open.name
        )));
    }
    let mut root =
        root.ok_or_else(|| MirthError::Xml("the document has no root element".into()))?;
    root.assign_locations("");
    Ok(root)
}

fn close(element: Element, stack: &mut [Element], root: &mut Option<Element>) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None => *root = Some(element),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_tree_with_locations_and_resolved_text() {
        let root = parse(
            "<?xml version=\"1.0\"?>\n<channel version=\"3.12.0\"><name>A &amp; B</name>\
             <list><connector><name>one</name></connector><connector><name>two</name></connector></list>\
             <script><![CDATA[if (a < b) {}]]>&#xd;\nx</script></channel>",
        )
        .unwrap();
        assert_eq!(root.attribute("version"), Some("3.12.0"));
        assert_eq!(root.value("name"), Some("A & B"));
        let list = root.child("list").unwrap();
        let second = list.children_named("connector").nth(1).unwrap();
        assert_eq!(second.location, "/channel/list/connector[2]");
        assert_eq!(
            second.child("name").unwrap().location,
            "/channel/list/connector[2]/name"
        );
        assert_eq!(root.script("script"), Some("if (a < b) {}\r\nx"));
    }

    #[test]
    fn rejects_document_types_and_broken_documents() {
        assert!(parse("<!DOCTYPE x [<!ENTITY a \"b\">]><x>&a;</x>").is_err());
        assert!(parse("<x>&unknown;</x>").is_err());
        assert!(parse("<x><y></x>").is_err());
        assert!(parse("").is_err());
        let deep = "<a>".repeat(MAX_DEPTH + 1);
        assert!(parse(&deep).is_err());
    }
}
