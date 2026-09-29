//! The element tree of a POCT1-A message.

use crate::error::BuildError;

/// An attribute of an [`Element`], in document order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attribute {
    /// The attribute name, for example `V`.
    pub name: String,
    /// The attribute value with references resolved and XML attribute-value
    /// normalization applied.
    pub value: String,
}

/// A child of an [`Element`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    /// A child element.
    Element(Element),
    /// Character data (text and CDATA sections, with references resolved).
    /// Adjacent character data is merged into one node.
    Text(String),
}

/// An XML element: name, attributes and children in document order.
///
/// POCT1-A carries most values in a `V` attribute (`<HDR.control_id V="1"/>`);
/// [`Element::value`] reads it. Comments and processing instructions are not
/// part of the tree; they remain in the original bytes of a parsed
/// [`Message`](crate::Message).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Element {
    name: String,
    attributes: Vec<Attribute>,
    children: Vec<Node>,
}

impl Element {
    /// Creates an element without attributes or children. The name is
    /// validated when the element is turned into a message.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Creates `<name V="value"/>`, the usual POCT1-A value element.
    pub fn with_v(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(name).attribute("V", value)
    }

    /// Adds or replaces an attribute (builder style).
    pub fn attribute(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.set_attribute(name, value);
        self
    }

    /// Appends a child element (builder style).
    pub fn child(mut self, child: Element) -> Self {
        self.push_child(child);
        self
    }

    /// Appends character data (builder style).
    pub fn text(mut self, text: impl AsRef<str>) -> Self {
        self.push_text(text.as_ref());
        self
    }

    /// Adds an attribute, or replaces the value of an existing attribute
    /// with the same name.
    pub fn set_attribute(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        match self.attributes.iter_mut().find(|a| a.name == name) {
            Some(existing) => existing.value = value,
            None => self.attributes.push(Attribute { name, value }),
        }
    }

    /// Appends a child element.
    pub fn push_child(&mut self, child: Element) {
        self.children.push(Node::Element(child));
    }

    /// Appends character data, merging it with preceding character data.
    /// Empty text is ignored.
    pub fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.children.last_mut() {
            Some(Node::Text(existing)) => existing.push_str(text),
            _ => self.children.push(Node::Text(text.to_owned())),
        }
    }

    /// The element name, for example `OBS.value`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The attributes in document order.
    pub fn attributes(&self) -> &[Attribute] {
        &self.attributes
    }

    /// The value of attribute `name`.
    pub fn attribute_value(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.as_str())
    }

    /// The value of the `V` attribute.
    pub fn value(&self) -> Option<&str> {
        self.attribute_value("V")
    }

    /// The children in document order.
    pub fn children(&self) -> &[Node] {
        &self.children
    }

    /// The child elements in document order.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        })
    }

    /// The first child element named `name`.
    pub fn first(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    /// The child elements named `name`.
    pub fn elements_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.elements().filter(move |e| e.name == name)
    }

    /// The direct character data of this element, concatenated.
    pub fn text_content(&self) -> String {
        self.children
            .iter()
            .filter_map(|node| match node {
                Node::Text(text) => Some(text.as_str()),
                Node::Element(_) => None,
            })
            .collect()
    }

    /// This element and all its descendants, depth first in document order.
    pub fn descendants(&self) -> Descendants<'_> {
        Descendants { stack: vec![self] }
    }

    /// The first element matching a path relative to this element.
    ///
    /// A path is a `/`-separated list of element names, each optionally
    /// followed by a 1-based index among same-named siblings:
    /// `SVC/PT/OBS[2]/OBS.value`. Returns `None` for an invalid path.
    pub fn find(&self, path: &str) -> Option<&Element> {
        self.find_all(path).into_iter().next()
    }

    /// All elements matching a path relative to this element, in document
    /// order. Segments without an index match every same-named child, so
    /// `SVC/PT/OBS` returns the observations of every patient of every
    /// service.
    pub fn find_all(&self, path: &str) -> Vec<&Element> {
        let Some(steps) = parse_path(path) else {
            return Vec::new();
        };
        let mut current = vec![self];
        for (name, index) in steps {
            let mut next = Vec::new();
            for element in current {
                let mut matching = element.elements().filter(|e| e.name == name);
                match index {
                    Some(index) => next.extend(matching.nth(index - 1)),
                    None => next.extend(matching),
                }
            }
            current = next;
        }
        current
    }

    /// The `V` attribute of the first element matching `path`.
    pub fn value_at(&self, path: &str) -> Option<&str> {
        self.find(path).and_then(Element::value)
    }

    /// Checks that names are valid XML names, attribute names are unique and
    /// every character can be written in XML 1.0.
    pub fn validate(&self) -> Result<(), BuildError> {
        for element in self.descendants() {
            if !is_xml_name(&element.name) {
                return Err(BuildError::InvalidName(element.name.clone()));
            }
            for (i, attribute) in element.attributes.iter().enumerate() {
                if !is_xml_name(&attribute.name) {
                    return Err(BuildError::InvalidName(attribute.name.clone()));
                }
                if element.attributes[..i]
                    .iter()
                    .any(|a| a.name == attribute.name)
                {
                    return Err(BuildError::DuplicateAttribute {
                        element: element.name.clone(),
                        attribute: attribute.name.clone(),
                    });
                }
                check_chars(&attribute.value, || {
                    format!("attribute {} of <{}>", attribute.name, element.name)
                })?;
            }
            for node in &element.children {
                if let Node::Text(text) = node {
                    check_chars(text, || format!("text of <{}>", element.name))?;
                }
            }
        }
        Ok(())
    }

    /// Serializes the element. Call [`Element::validate`] first; invalid
    /// names or characters produce XML that does not parse.
    pub(crate) fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for attribute in &self.attributes {
            out.push(' ');
            out.push_str(&attribute.name);
            out.push_str("=\"");
            escape_into(&attribute.value, true, out);
            out.push('"');
        }
        if self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for node in &self.children {
            match node {
                Node::Element(child) => child.write(out),
                Node::Text(text) => escape_into(text, false, out),
            }
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push('>');
    }
}

/// Iterator over an element and its descendants, depth first.
#[derive(Debug, Clone)]
pub struct Descendants<'a> {
    stack: Vec<&'a Element>,
}

impl<'a> Iterator for Descendants<'a> {
    type Item = &'a Element;

    fn next(&mut self) -> Option<&'a Element> {
        let element = self.stack.pop()?;
        let children: Vec<&Element> = element.elements().collect();
        self.stack.extend(children.into_iter().rev());
        Some(element)
    }
}

/// Parses `A/B[2]/C` into `[("A", None), ("B", Some(2)), ("C", None)]`.
fn parse_path(path: &str) -> Option<Vec<(&str, Option<usize>)>> {
    path.split('/')
        .map(|step| {
            let (name, index) = match step.split_once('[') {
                Some((name, rest)) => {
                    let index: usize = rest.strip_suffix(']')?.parse().ok()?;
                    if index == 0 {
                        return None;
                    }
                    (name, Some(index))
                }
                None => (step, None),
            };
            is_xml_name(name).then_some((name, index))
        })
        .collect()
}

/// A simplified XML `Name`: an ASCII letter, `_`, `:` or a non-ASCII
/// alphabetic character, followed by ASCII letters, digits, `.`, `-`, `_`,
/// `:` or non-ASCII characters that are neither whitespace nor control
/// characters.
pub(crate) fn is_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let start_ok = first.is_ascii_alphabetic()
        || first == '_'
        || first == ':'
        || (!first.is_ascii() && first.is_alphabetic());
    start_ok
        && chars.all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '-' | '_' | ':')
                || (!c.is_ascii() && !c.is_whitespace() && !c.is_control() && is_xml_char(c))
        })
}

/// Whether `c` is allowed in an XML 1.0 document.
pub(crate) fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

fn check_chars(text: &str, context: impl FnOnce() -> String) -> Result<(), BuildError> {
    match text.chars().find(|&c| !is_xml_char(c)) {
        Some(c) => Err(BuildError::InvalidCharacter {
            context: context(),
            code: u32::from(c),
        }),
        None => Ok(()),
    }
}

/// Escapes markup characters. Whitespace that XML parsers normalize
/// (`\r` everywhere; `\t` and `\n` in attributes) is written as character
/// references so values survive a round trip exactly.
fn escape_into(text: &str, attribute: bool, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\t' if attribute => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Element {
        Element::new("OBS.R01")
            .child(Element::new("HDR").child(Element::with_v("HDR.control_id", "7")))
            .child(
                Element::new("SVC")
                    .child(
                        Element::new("PT")
                            .child(Element::with_v("PT.patient_id", "P1"))
                            .child(
                                Element::new("OBS")
                                    .child(Element::with_v("OBS.observation_id", "GLU")),
                            )
                            .child(
                                Element::new("OBS")
                                    .child(Element::with_v("OBS.observation_id", "NA")),
                            ),
                    )
                    .child(Element::new("PT").child(
                        Element::new("OBS").child(Element::with_v("OBS.observation_id", "K")),
                    )),
            )
    }

    #[test]
    fn finds_paths() {
        let root = sample();
        assert_eq!(root.value_at("HDR/HDR.control_id"), Some("7"));
        let ids: Vec<_> = root
            .find_all("SVC/PT/OBS/OBS.observation_id")
            .into_iter()
            .filter_map(Element::value)
            .collect();
        assert_eq!(ids, ["GLU", "NA", "K"]);
        assert_eq!(
            root.value_at("SVC/PT[1]/OBS[2]/OBS.observation_id"),
            Some("NA")
        );
        assert_eq!(root.value_at("SVC/PT[2]/OBS/OBS.observation_id"), Some("K"));
        assert!(root.find("SVC/PT[3]").is_none());
        assert!(root.find("SVC/PT[0]").is_none());
        assert!(root.find("SVC//PT").is_none());
        assert!(root.find("SVC/PT[x]").is_none());
    }

    #[test]
    fn walks_descendants_in_document_order() {
        let names: Vec<_> = sample()
            .descendants()
            .map(|e| e.name().to_owned())
            .collect();
        assert_eq!(names[..4], ["OBS.R01", "HDR", "HDR.control_id", "SVC"]);
        assert_eq!(names.len(), 13);
    }

    #[test]
    fn merges_text_and_replaces_attributes() {
        let element = Element::new("NTE")
            .text("a")
            .text("")
            .text("b")
            .attribute("V", "1")
            .attribute("V", "2");
        assert_eq!(element.children(), [Node::Text("ab".into())]);
        assert_eq!(element.value(), Some("2"));
        assert_eq!(element.attributes().len(), 1);
    }

    #[test]
    fn writes_escaped_xml() {
        let element = Element::new("NTE")
            .attribute("V", "a<b>&\"c\"\t\n\r")
            .text("x < y & z\r\n");
        let mut out = String::new();
        element.write(&mut out);
        assert_eq!(
            out,
            "<NTE V=\"a&lt;b&gt;&amp;&quot;c&quot;&#9;&#10;&#13;\">x &lt; y &amp; z&#13;\n</NTE>"
        );
    }

    #[test]
    fn validates_names_and_characters() {
        assert!(sample().validate().is_ok());
        assert!(matches!(
            Element::new("1bad").validate(),
            Err(BuildError::InvalidName(_))
        ));
        assert!(matches!(
            Element::new("A").attribute("bad name", "x").validate(),
            Err(BuildError::InvalidName(_))
        ));
        assert!(matches!(
            Element::new("A").attribute("V", "\u{1}").validate(),
            Err(BuildError::InvalidCharacter { code: 1, .. })
        ));
        assert!(matches!(
            Element::new("A").text("\u{FFFE}").validate(),
            Err(BuildError::InvalidCharacter { .. })
        ));
        let mut duplicate = Element::new("A");
        duplicate.attributes.push(Attribute {
            name: "V".into(),
            value: "1".into(),
        });
        duplicate.attributes.push(Attribute {
            name: "V".into(),
            value: "2".into(),
        });
        assert!(matches!(
            duplicate.validate(),
            Err(BuildError::DuplicateAttribute { .. })
        ));
    }

    #[test]
    fn recognizes_xml_names() {
        for name in [
            "HDR",
            "OBS.value",
            "DEV.device_id",
            "_x",
            "a-b",
            "ns:tag",
            "Ölçüm",
        ] {
            assert!(is_xml_name(name), "{name}");
        }
        for name in ["", "1a", ".a", "-a", "a b", "a/b", "a[1]"] {
            assert!(!is_xml_name(name), "{name}");
        }
    }
}
