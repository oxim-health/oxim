//! XML documents.
//!
//! The parser keeps every token of the original document (declaration,
//! comments, processing instructions, whitespace, attribute quoting, entity
//! references, CDATA sections), so an unmodified document serializes byte
//! for byte and an edit rewrites only the start tag or text it touches.
//!
//! Document type declarations and custom entities are rejected: only the
//! five predefined entities and character references are accepted. This
//! rules out entity expansion attacks and external entity resolution.
//!
//! The encoding comes from the XML declaration (UTF-8 by default). UTF-16
//! and encodings whose multi-byte characters may contain ASCII bytes are not
//! supported.

use std::fmt;
use std::str::FromStr;

use encoding_rs::{Encoding, UTF_8};
use memchr::{memchr, memmem};
use thiserror::Error;

use crate::text::{decode, is_byte_safe};

/// The largest accepted positional index in a path and the longest accepted
/// name.
const MAX_NAME_LEN: usize = 1024;
const MAX_PATH_STEPS: usize = 256;
const MAX_INDEX: usize = 4096;

/// Options for [`XmlDocument::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct XmlOptions {
    /// The largest accepted document, in bytes.
    pub max_len: usize,
    /// The deepest accepted element nesting.
    pub max_depth: usize,
    /// The largest accepted number of elements.
    pub max_elements: usize,
    /// The largest accepted number of attributes on one element.
    pub max_attributes: usize,
}

impl Default for XmlOptions {
    fn default() -> Self {
        Self {
            max_len: 64 * 1024 * 1024,
            max_depth: 256,
            max_elements: 1_000_000,
            max_attributes: 256,
        }
    }
}

/// Returned when XML cannot be parsed or a path cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum XmlError {
    /// The document is not well-formed.
    #[error("malformed XML at byte {offset}: {reason}")]
    Syntax {
        /// Byte offset of the problem.
        offset: usize,
        /// What is wrong.
        reason: &'static str,
    },
    /// The document contains a document type declaration.
    #[error("document type declarations are not allowed (byte {0})")]
    DocumentType(usize),
    /// The document references an entity other than the predefined ones.
    #[error("entity reference {0:?} is not allowed")]
    Entity(String),
    /// The document exceeds a limit from [`XmlOptions`].
    #[error("XML exceeds the configured limit for {0}")]
    LimitExceeded(&'static str),
    /// The declared encoding is unknown or unsupported.
    #[error("unsupported XML encoding {0:?}")]
    UnsupportedEncoding(String),
    /// The path text is not a valid path.
    #[error("invalid XML path {0:?}")]
    Path(String),
    /// The path cannot be applied to the document.
    #[error("XML path {0:?} does not match the document")]
    NotFound(String),
    /// The value contains a character that XML 1.0 cannot represent, such as
    /// a control character.
    #[error("value contains a character that XML 1.0 cannot represent")]
    InvalidCharacter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Element(Element),
    /// Character data, still escaped, exactly as written.
    Text(Vec<u8>),
    /// A whole CDATA section including its markers.
    CData(Vec<u8>),
    /// Comments, processing instructions, the XML declaration and a byte
    /// order mark, kept verbatim.
    Other(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Element {
    /// The qualified name as written.
    name: Vec<u8>,
    attributes: Vec<Attribute>,
    /// Everything after the last attribute up to and including `>` or `/>`.
    tail: Vec<u8>,
    self_closing: bool,
    children: Vec<usize>,
    /// The end tag as written; empty for self-closing elements.
    end: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribute {
    /// Leading whitespace, name, `=`, quotes and value, as written.
    raw: Vec<u8>,
    name: Vec<u8>,
    /// Position of the (escaped) value inside `raw`.
    value: std::ops::Range<usize>,
    quote: u8,
}

/// A parsed XML document.
#[derive(Debug, Clone, PartialEq)]
pub struct XmlDocument {
    nodes: Vec<Node>,
    /// Top-level nodes in document order: prolog, root element, epilog.
    top: Vec<usize>,
    root: usize,
    encoding: &'static Encoding,
}

impl XmlDocument {
    /// Parses a document.
    pub fn parse(input: &[u8], options: &XmlOptions) -> Result<Self, XmlError> {
        Parser::new(input, options).document()
    }

    /// The document encoding.
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// The qualified name of the root element.
    pub fn root_name(&self) -> String {
        match &self.nodes[self.root] {
            Node::Element(element) => decode(&element.name, self.encoding),
            _ => String::new(),
        }
    }

    /// The text or attribute value at `path`, or `None` when it is absent.
    /// See [`XmlPath`] for the syntax.
    pub fn get(&self, path: &str) -> Result<Option<String>, XmlError> {
        Ok(self.get_path(&path.parse()?))
    }

    /// The text or attribute value at `path`, or `None` when it is absent.
    ///
    /// The text of an element is the concatenation of its direct text and
    /// CDATA children, with references resolved.
    pub fn get_path(&self, path: &XmlPath) -> Option<String> {
        let element = self.resolve(path)?;
        match &path.target {
            Target::Text => Some(self.text_of(element)),
            Target::Attribute(name) => {
                let attribute = self.find_attribute(element, name)?;
                let raw = &attribute.raw[attribute.value.clone()];
                Some(resolve_references(&normalize_attribute(&decode(
                    raw,
                    self.encoding,
                ))))
            }
        }
    }

    /// The number of elements matching the element steps of `path`, ignoring
    /// the positional index of its last element step. Useful to iterate
    /// repeated elements such as `/order/test`.
    pub fn count(&self, path: &str) -> Result<usize, XmlError> {
        let mut path: XmlPath = path.parse()?;
        let Some(last) = path.steps.pop() else {
            return Ok(0);
        };
        if path.steps.is_empty() {
            return Ok(usize::from(self.matches_name(&last.name, self.root)));
        }
        Ok(self
            .resolve(&path)
            .map_or(0, |parent| self.children_named(parent, &last.name).count()))
    }

    /// Stores `value` at `path`, creating missing elements and attributes.
    ///
    /// A missing element is created when its positional index is one past
    /// the last existing match. Writing an element's text replaces all of its
    /// direct text and CDATA children with one text node.
    pub fn set(&mut self, path: &str, value: &str) -> Result<(), XmlError> {
        self.set_path(&path.parse()?, value)
    }

    /// Stores `value` at `path`, creating missing elements and attributes.
    pub fn set_path(&mut self, path: &XmlPath, value: &str) -> Result<(), XmlError> {
        if !value.chars().all(is_xml_char) {
            return Err(XmlError::InvalidCharacter);
        }
        let not_found = || XmlError::NotFound(path.text.clone());
        let (first, rest) = path.steps.split_first().ok_or_else(not_found)?;
        if first.index != 1 || !self.matches_name(&first.name, self.root) {
            return Err(not_found());
        }
        let mut current = self.root;
        for step in rest {
            let matches: Vec<usize> = self.children_named(current, &step.name).collect();
            current = match matches.get(step.index - 1) {
                Some(&id) => id,
                None if matches.len() == step.index - 1 && step.name.local != "*" => {
                    self.create_child(current, &step.name)?
                }
                None => return Err(not_found()),
            };
        }
        match &path.target {
            Target::Text => self.set_text(current, value),
            Target::Attribute(name) => self.set_attribute(current, name, value),
        }
    }

    /// Serializes the document. Unmodified documents are reproduced byte for
    /// byte.
    pub fn to_bytes(&self) -> Vec<u8> {
        enum Step {
            Open(usize),
            Close(usize),
        }
        let mut out = Vec::new();
        let mut stack: Vec<Step> = self.top.iter().rev().map(|&id| Step::Open(id)).collect();
        while let Some(step) = stack.pop() {
            match step {
                Step::Open(id) => match &self.nodes[id] {
                    Node::Element(element) => {
                        out.push(b'<');
                        out.extend_from_slice(&element.name);
                        for attribute in &element.attributes {
                            out.extend_from_slice(&attribute.raw);
                        }
                        out.extend_from_slice(&element.tail);
                        stack.push(Step::Close(id));
                        stack.extend(
                            element
                                .children
                                .iter()
                                .rev()
                                .map(|&child| Step::Open(child)),
                        );
                    }
                    Node::Text(bytes) | Node::CData(bytes) | Node::Other(bytes) => {
                        out.extend_from_slice(bytes);
                    }
                },
                Step::Close(id) => {
                    if let Node::Element(element) = &self.nodes[id] {
                        out.extend_from_slice(&element.end);
                    }
                }
            }
        }
        out
    }

    fn element(&self, id: usize) -> Option<&Element> {
        match self.nodes.get(id) {
            Some(Node::Element(element)) => Some(element),
            _ => None,
        }
    }

    fn element_mut(&mut self, id: usize) -> Option<&mut Element> {
        match self.nodes.get_mut(id) {
            Some(Node::Element(element)) => Some(element),
            _ => None,
        }
    }

    fn matches_name(&self, name: &Name, id: usize) -> bool {
        self.element(id)
            .is_some_and(|element| name.matches(&decode(&element.name, self.encoding)))
    }

    fn children_named<'a>(
        &'a self,
        parent: usize,
        name: &'a Name,
    ) -> impl Iterator<Item = usize> + 'a {
        self.element(parent)
            .map(|element| element.children.as_slice())
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(move |&child| self.matches_name(name, child))
    }

    fn resolve(&self, path: &XmlPath) -> Option<usize> {
        let (first, rest) = path.steps.split_first()?;
        if first.index != 1 || !self.matches_name(&first.name, self.root) {
            return None;
        }
        let mut current = self.root;
        for step in rest {
            current = self
                .children_named(current, &step.name)
                .nth(step.index - 1)?;
        }
        Some(current)
    }

    fn text_of(&self, id: usize) -> String {
        let mut text = String::new();
        for &child in self
            .element(id)
            .map(|e| e.children.as_slice())
            .unwrap_or_default()
        {
            match &self.nodes[child] {
                Node::Text(raw) => text.push_str(&resolve_references(&decode(raw, self.encoding))),
                Node::CData(raw) => {
                    let inner = raw.get(9..raw.len().saturating_sub(3)).unwrap_or_default();
                    text.push_str(&decode(inner, self.encoding));
                }
                _ => {}
            }
        }
        text
    }

    fn find_attribute(&self, id: usize, name: &Name) -> Option<&Attribute> {
        let element = self.element(id)?;
        let names: Vec<String> = element
            .attributes
            .iter()
            .map(|attribute| decode(&attribute.name, self.encoding))
            .collect();
        // An exact qualified-name match wins over a local-name match.
        let position = names
            .iter()
            .position(|n| name.prefix.is_none() && *n == name.local)
            .or_else(|| names.iter().position(|n| name.matches(n)))?;
        element.attributes.get(position)
    }

    fn encode(&self, text: &str) -> Vec<u8> {
        // Unmappable characters become numeric character references, which
        // are valid XML.
        self.encoding.encode(text).0.into_owned()
    }

    fn create_child(&mut self, parent: usize, name: &Name) -> Result<usize, XmlError> {
        let qualified = name.qualified();
        if !is_valid_name(&qualified) {
            return Err(XmlError::Path(qualified));
        }
        let element = Element {
            name: self.encode(&qualified),
            attributes: Vec::new(),
            tail: b"/>".to_vec(),
            self_closing: true,
            children: Vec::new(),
            end: Vec::new(),
        };
        let id = self.nodes.len();
        self.nodes.push(Node::Element(element));
        self.open(parent);
        if let Some(parent) = self.element_mut(parent) {
            parent.children.push(id);
        }
        Ok(id)
    }

    /// Turns a self-closing element into one with an end tag.
    fn open(&mut self, id: usize) {
        if let Some(element) = self.element_mut(id)
            && element.self_closing
        {
            element.tail.truncate(element.tail.len().saturating_sub(2));
            element.tail.push(b'>');
            element.end = [&b"</"[..], element.name.as_slice(), &b">"[..]].concat();
            element.self_closing = false;
        }
    }

    fn set_text(&mut self, id: usize, value: &str) -> Result<(), XmlError> {
        let bytes = self.encode(&escape_text(value));
        let text_node = (!bytes.is_empty()).then(|| {
            self.nodes.push(Node::Text(bytes));
            self.nodes.len() - 1
        });
        if text_node.is_some() {
            self.open(id);
        }
        let is_text: Vec<bool> = self
            .element(id)
            .map(|e| e.children.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|&child| matches!(self.nodes[child], Node::Text(_) | Node::CData(_)))
            .collect();
        let Some(element) = self.element_mut(id) else {
            return Ok(());
        };
        let position = is_text
            .iter()
            .position(|&t| t)
            .unwrap_or(element.children.len());
        let mut flags = is_text.iter();
        element
            .children
            .retain(|_| !flags.next().copied().unwrap_or(false));
        if let Some(node) = text_node {
            let position = position.min(element.children.len());
            element.children.insert(position, node);
        }
        Ok(())
    }

    fn set_attribute(&mut self, id: usize, name: &Name, value: &str) -> Result<(), XmlError> {
        let existing = self
            .find_attribute(id, name)
            .map(|a| (a.name.clone(), a.quote));
        let encoding = self.encoding;
        let Some(element) = self.element_mut(id) else {
            return Ok(());
        };
        match existing {
            Some((attribute_name, quote)) => {
                let escaped = encoding
                    .encode(&escape_attribute(value, quote))
                    .0
                    .into_owned();
                if let Some(attribute) = element
                    .attributes
                    .iter_mut()
                    .find(|a| a.name == attribute_name)
                {
                    let range = attribute.value.clone();
                    let mut raw = attribute.raw[..range.start].to_vec();
                    raw.extend_from_slice(&escaped);
                    raw.extend_from_slice(&attribute.raw[range.end..]);
                    attribute.raw = raw;
                    attribute.value = range.start..range.start + escaped.len();
                }
            }
            None => {
                let qualified = name.qualified();
                if name.local == "*" || !is_valid_name(&qualified) {
                    return Err(XmlError::Path(qualified));
                }
                let name_bytes = encoding.encode(&qualified).0.into_owned();
                let escaped = encoding
                    .encode(&escape_attribute(value, b'"'))
                    .0
                    .into_owned();
                let mut raw = Vec::with_capacity(name_bytes.len() + escaped.len() + 4);
                raw.push(b' ');
                raw.extend_from_slice(&name_bytes);
                raw.extend_from_slice(b"=\"");
                let start = raw.len();
                raw.extend_from_slice(&escaped);
                raw.push(b'"');
                element.attributes.push(Attribute {
                    raw,
                    name: name_bytes,
                    value: start..start + escaped.len(),
                    quote: b'"',
                });
            }
        }
        Ok(())
    }
}

impl XmlDocument {
    /// The root element.
    pub fn root(&self) -> XmlElement<'_> {
        XmlElement {
            document: self,
            id: self.root,
        }
    }

    /// The element at `path`, or `None` when it is absent. An attribute
    /// step at the end of the path is not allowed.
    pub fn element_at(&self, path: &str) -> Result<Option<XmlElement<'_>>, XmlError> {
        let path: XmlPath = path.parse()?;
        if matches!(path.target, Target::Attribute(_)) {
            return Err(XmlError::Path(path.text));
        }
        Ok(self
            .resolve(&path)
            .map(|id| XmlElement { document: self, id }))
    }
}

/// A read-only view of one element, for walking a document without
/// building paths.
///
/// Names follow the path rules: an unprefixed name matches the local name
/// whatever the prefix, a prefixed name must match exactly, and namespace
/// URIs are not resolved.
#[derive(Clone, Copy)]
pub struct XmlElement<'a> {
    document: &'a XmlDocument,
    id: usize,
}

impl fmt::Debug for XmlElement<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XmlElement")
            .field("name", &self.name())
            .finish_non_exhaustive()
    }
}

impl<'a> XmlElement<'a> {
    fn data(&self) -> Option<&'a Element> {
        self.document.element(self.id)
    }

    /// The qualified name as written, for example `cda:section`.
    pub fn name(&self) -> String {
        self.data()
            .map(|e| decode(&e.name, self.document.encoding))
            .unwrap_or_default()
    }

    /// The name without its prefix.
    pub fn local_name(&self) -> String {
        let name = self.name();
        match name.split_once(':') {
            Some((_, local)) => local.to_owned(),
            None => name,
        }
    }

    /// Whether the element's name matches `name`.
    pub fn is(&self, name: &str) -> bool {
        Name::parse(name).is_some_and(|name| self.document.matches_name(&name, self.id))
    }

    /// The value of attribute `name` with references resolved, for example
    /// `code` or `xsi:type`.
    pub fn attribute(&self, name: &str) -> Option<String> {
        let name = Name::parse(name).filter(|n| n.local != "*")?;
        let attribute = self.document.find_attribute(self.id, &name)?;
        let raw = &attribute.raw[attribute.value.clone()];
        Some(resolve_references(&normalize_attribute(&decode(
            raw,
            self.document.encoding,
        ))))
    }

    /// The child elements in document order.
    pub fn children(&self) -> impl Iterator<Item = XmlElement<'a>> + use<'a> {
        let document = self.document;
        self.data()
            .map(|e| e.children.as_slice())
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(move |&id| document.element(id).is_some())
            .map(move |id| XmlElement { document, id })
    }

    /// The child elements named `name`.
    pub fn children_named(&self, name: &str) -> impl Iterator<Item = XmlElement<'a>> + use<'a> {
        let name = Name::parse(name);
        self.children().filter(move |child| {
            name.as_ref()
                .is_some_and(|n| child.document.matches_name(n, child.id))
        })
    }

    /// The first child element named `name`.
    pub fn child(&self, name: &str) -> Option<XmlElement<'a>> {
        self.children_named(name).next()
    }

    /// The element's own text: its direct text and CDATA children, with
    /// references resolved.
    pub fn text(&self) -> String {
        self.document.text_of(self.id)
    }

    /// All text inside the element, including that of descendants, in
    /// document order.
    pub fn text_content(&self) -> String {
        self.text_content_with(|_| false)
    }

    /// All text inside the element, with a space before and after the text
    /// of every descendant element whose local name `separates` accepts,
    /// for example table cells and paragraphs of a narrative.
    pub fn text_content_with(&self, separates: impl Fn(&str) -> bool) -> String {
        enum Step {
            Visit(usize),
            Space,
        }
        let mut text = String::new();
        let mut stack = vec![Step::Visit(self.id)];
        while let Some(step) = stack.pop() {
            let id = match step {
                Step::Visit(id) => id,
                Step::Space => {
                    text.push(' ');
                    continue;
                }
            };
            match &self.document.nodes[id] {
                Node::Element(element) => {
                    let name = decode(&element.name, self.document.encoding);
                    let local = name.rsplit(':').next().unwrap_or_default();
                    let spaced = id != self.id && separates(local);
                    if spaced {
                        text.push(' ');
                        stack.push(Step::Space);
                    }
                    stack.extend(
                        element
                            .children
                            .iter()
                            .rev()
                            .map(|&child| Step::Visit(child)),
                    );
                }
                Node::Text(raw) => {
                    text.push_str(&resolve_references(&decode(raw, self.document.encoding)));
                }
                Node::CData(raw) => {
                    let inner = raw.get(9..raw.len().saturating_sub(3)).unwrap_or_default();
                    text.push_str(&decode(inner, self.document.encoding));
                }
                Node::Other(_) => {}
            }
        }
        text
    }
}

/// A name in a path: `local`, `prefix:local` or the wildcard `*`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Name {
    prefix: Option<String>,
    local: String,
}

impl Name {
    fn parse(text: &str) -> Option<Self> {
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| !c.is_whitespace() && !"/[]@()=\"'<>&:".contains(c))
        };
        let (prefix, local) = match text.split_once(':') {
            Some((prefix, local)) => (Some(prefix), local),
            None => (None, text),
        };
        let local_ok = local == "*" || valid(local);
        (local_ok && prefix.is_none_or(valid)).then(|| Self {
            prefix: prefix.map(str::to_owned),
            local: local.to_owned(),
        })
    }

    /// Unprefixed names match on the local part only; prefixed names must
    /// match the qualified name exactly. Namespace URIs are not resolved.
    fn matches(&self, qualified: &str) -> bool {
        let (prefix, local) = match qualified.split_once(':') {
            Some((prefix, local)) => (Some(prefix), local),
            None => (None, qualified),
        };
        let local_ok = self.local == "*" || self.local == local;
        match &self.prefix {
            None => local_ok,
            Some(p) => local_ok && prefix == Some(p.as_str()),
        }
    }

    fn qualified(&self) -> String {
        match &self.prefix {
            Some(prefix) => format!("{prefix}:{}", self.local),
            None => self.local.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Step {
    name: Name,
    /// 1-based position among the siblings matching `name`.
    index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Target {
    Text,
    Attribute(Name),
}

/// A location in an XML document, written in a small subset of XPath:
///
/// - `/order/test[2]` — elements from the root; `[n]` is a 1-based position
///   among siblings with that name (default 1).
/// - `/order/test[2]/@code` — an attribute.
/// - `/order/test/text()` — the text of an element (the same as the element
///   path itself).
/// - `/cda:ClinicalDocument/cda:title` — prefixed steps match the qualified
///   name exactly; unprefixed steps match the local name whatever the prefix.
///   Namespace URIs are not resolved.
/// - `*` matches any element name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct XmlPath {
    text: String,
    steps: Vec<Step>,
    target: Target,
}

impl FromStr for XmlPath {
    type Err = XmlError;

    fn from_str(s: &str) -> Result<Self, XmlError> {
        let invalid = || XmlError::Path(s.to_owned());
        let body = s.strip_prefix('/').ok_or_else(invalid)?;
        let parts: Vec<&str> = body.split('/').collect();
        if parts.len() > MAX_PATH_STEPS {
            return Err(invalid());
        }
        let (last, elements) = parts.split_last().ok_or_else(invalid)?;
        let (target, element_parts) = if *last == "text()" {
            (Target::Text, elements)
        } else if let Some(attribute) = last.strip_prefix('@') {
            let name = Name::parse(attribute)
                .filter(|n| n.local != "*")
                .ok_or_else(invalid)?;
            (Target::Attribute(name), elements)
        } else {
            (Target::Text, &parts[..])
        };
        if element_parts.is_empty() {
            return Err(invalid());
        }
        let mut steps = Vec::with_capacity(element_parts.len());
        for part in element_parts {
            let (name, index) = match part.split_once('[') {
                Some((name, rest)) => {
                    let digits = rest.strip_suffix(']').ok_or_else(invalid)?;
                    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(invalid());
                    }
                    let index = digits.parse::<usize>().map_err(|_| invalid())?;
                    if !(1..=MAX_INDEX).contains(&index) {
                        return Err(invalid());
                    }
                    (name, index)
                }
                None => (*part, 1),
            };
            steps.push(Step {
                name: Name::parse(name).ok_or_else(invalid)?,
                index,
            });
        }
        Ok(Self {
            text: s.to_owned(),
            steps,
            target,
        })
    }
}

impl fmt::Display for XmlPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    options: &'a XmlOptions,
    nodes: Vec<Node>,
    elements: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8], options: &'a XmlOptions) -> Self {
        Self {
            input,
            pos: 0,
            options,
            nodes: Vec::new(),
            elements: 0,
        }
    }

    fn document(mut self) -> Result<XmlDocument, XmlError> {
        if self.input.len() > self.options.max_len {
            return Err(XmlError::LimitExceeded("document size"));
        }
        if self.input.starts_with(&[0xFF, 0xFE]) || self.input.starts_with(&[0xFE, 0xFF]) {
            return Err(XmlError::UnsupportedEncoding("UTF-16".into()));
        }
        let input = self.input;
        let mut top = Vec::new();
        let mut encoding = UTF_8;
        if self.starts_with(b"\xEF\xBB\xBF") {
            top.push(self.push(Node::Other(b"\xEF\xBB\xBF".to_vec())));
            self.pos = 3;
        }
        if self.starts_with(b"<?xml")
            && input
                .get(self.pos + 5)
                .is_some_and(|&b| is_whitespace(b) || b == b'?')
        {
            let start = self.pos;
            let end = self.find(b"?>", 2, "unterminated XML declaration")?;
            let declaration = &input[start..end];
            if let Some(label) = declared_encoding(declaration) {
                encoding = Encoding::for_label(label)
                    .filter(|&e| is_byte_safe(e))
                    .ok_or_else(|| {
                        XmlError::UnsupportedEncoding(String::from_utf8_lossy(label).into_owned())
                    })?;
            }
            top.push(self.push(Node::Other(declaration.to_vec())));
        }
        let root = loop {
            match self.input.get(self.pos) {
                None => return Err(self.error("missing root element")),
                Some(&b) if is_whitespace(b) => {
                    let id = self.whitespace();
                    top.push(id);
                }
                Some(b'<') if self.starts_with(b"<!--") || self.starts_with(b"<?") => {
                    let id = self.markup()?;
                    top.push(id);
                }
                Some(b'<') if self.starts_with(b"<!") => {
                    return Err(XmlError::DocumentType(self.pos));
                }
                Some(b'<') => break self.element_tree()?,
                Some(_) => return Err(self.error("text outside the root element")),
            }
        };
        top.push(root);
        while let Some(&b) = self.input.get(self.pos) {
            let id = if is_whitespace(b) {
                self.whitespace()
            } else if self.starts_with(b"<!--") || self.starts_with(b"<?") {
                self.markup()?
            } else if self.starts_with(b"<!") {
                return Err(XmlError::DocumentType(self.pos));
            } else {
                return Err(self.error("content after the root element"));
            };
            top.push(id);
        }
        Ok(XmlDocument {
            nodes: self.nodes,
            top,
            root,
            encoding,
        })
    }

    fn error(&self, reason: &'static str) -> XmlError {
        XmlError::Syntax {
            offset: self.pos,
            reason,
        }
    }

    fn starts_with(&self, prefix: &[u8]) -> bool {
        self.input
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(prefix))
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Finds `terminator` after skipping `skip` bytes and returns the offset
    /// just past it.
    fn find(
        &mut self,
        terminator: &[u8],
        skip: usize,
        reason: &'static str,
    ) -> Result<usize, XmlError> {
        let from = (self.pos + skip).min(self.input.len());
        let at = memmem::find(&self.input[from..], terminator).ok_or_else(|| self.error(reason))?;
        let end = from + at + terminator.len();
        self.pos = end;
        Ok(end)
    }

    fn whitespace(&mut self) -> usize {
        let start = self.pos;
        while self.input.get(self.pos).is_some_and(|&b| is_whitespace(b)) {
            self.pos += 1;
        }
        self.push(Node::Text(self.input[start..self.pos].to_vec()))
    }

    /// Parses a comment, processing instruction or CDATA section.
    fn markup(&mut self) -> Result<usize, XmlError> {
        let start = self.pos;
        let node = if self.starts_with(b"<!--") {
            let end = self.find(b"-->", 4, "unterminated comment")?;
            Node::Other(self.input[start..end].to_vec())
        } else if self.starts_with(b"<![CDATA[") {
            let end = self.find(b"]]>", 9, "unterminated CDATA section")?;
            Node::CData(self.input[start..end].to_vec())
        } else {
            self.pos += 2;
            let target = self.name()?;
            if target.eq_ignore_ascii_case(b"xml") {
                self.pos = start;
                return Err(self.error("the XML declaration must be at the start of the document"));
            }
            let end = self.find(b"?>", 0, "unterminated processing instruction")?;
            Node::Other(self.input[start..end].to_vec())
        };
        Ok(self.push(node))
    }

    fn name(&mut self) -> Result<&'a [u8], XmlError> {
        let start = self.pos;
        let input = self.input;
        if !input.get(self.pos).is_some_and(|&b| is_name_start(b)) {
            return Err(self.error("expected a name"));
        }
        while input.get(self.pos).is_some_and(|&b| is_name_char(b)) {
            self.pos += 1;
            if self.pos - start > MAX_NAME_LEN {
                return Err(XmlError::LimitExceeded("name length"));
            }
        }
        Ok(&input[start..self.pos])
    }

    fn skip_whitespace(&mut self) {
        while self.input.get(self.pos).is_some_and(|&b| is_whitespace(b)) {
            self.pos += 1;
        }
    }

    /// Parses the root element and everything inside it without recursion.
    fn element_tree(&mut self) -> Result<usize, XmlError> {
        let root = self.start_tag()?;
        let mut stack = Vec::new();
        if self.is_open(root) {
            stack.push(root);
        }
        while let Some(&current) = stack.last() {
            if self.pos >= self.input.len() {
                return Err(self.error("unclosed element"));
            }
            let child = if self.starts_with(b"</") {
                let end = self.end_tag(current)?;
                if let Some(Node::Element(element)) = self.nodes.get_mut(current) {
                    element.end = end;
                }
                stack.pop();
                continue;
            } else if self.starts_with(b"<!--")
                || self.starts_with(b"<?")
                || self.starts_with(b"<![CDATA[")
            {
                self.markup()?
            } else if self.starts_with(b"<!") {
                return Err(XmlError::DocumentType(self.pos));
            } else if self.starts_with(b"<") {
                if stack.len() >= self.options.max_depth {
                    return Err(XmlError::LimitExceeded("depth"));
                }
                self.start_tag()?
            } else {
                self.text()?
            };
            if let Some(Node::Element(element)) = self.nodes.get_mut(current) {
                element.children.push(child);
            }
            if self.is_open(child) {
                stack.push(child);
            }
        }
        Ok(root)
    }

    fn is_open(&self, id: usize) -> bool {
        matches!(self.nodes.get(id), Some(Node::Element(element)) if !element.self_closing)
    }

    fn start_tag(&mut self) -> Result<usize, XmlError> {
        self.pos += 1;
        let name = self.name()?.to_vec();
        let mut attributes: Vec<Attribute> = Vec::new();
        let (tail, self_closing) = loop {
            let whitespace_start = self.pos;
            self.skip_whitespace();
            match self.input.get(self.pos..).unwrap_or_default() {
                [b'>', ..] => {
                    self.pos += 1;
                    break (self.input[whitespace_start..self.pos].to_vec(), false);
                }
                [b'/', b'>', ..] => {
                    self.pos += 2;
                    break (self.input[whitespace_start..self.pos].to_vec(), true);
                }
                [] => return Err(self.error("unterminated start tag")),
                _ if whitespace_start == self.pos => {
                    return Err(self.error("expected whitespace before an attribute"));
                }
                _ => {
                    let attribute = self.attribute(whitespace_start)?;
                    if attributes.iter().any(|a| a.name == attribute.name) {
                        return Err(self.error("duplicate attribute"));
                    }
                    if attributes.len() >= self.options.max_attributes {
                        return Err(XmlError::LimitExceeded("attributes per element"));
                    }
                    attributes.push(attribute);
                }
            }
        };
        self.elements += 1;
        if self.elements > self.options.max_elements {
            return Err(XmlError::LimitExceeded("elements"));
        }
        Ok(self.push(Node::Element(Element {
            name,
            attributes,
            tail,
            self_closing,
            children: Vec::new(),
            end: Vec::new(),
        })))
    }

    fn attribute(&mut self, raw_start: usize) -> Result<Attribute, XmlError> {
        let name = self.name()?.to_vec();
        self.skip_whitespace();
        if self.input.get(self.pos) != Some(&b'=') {
            return Err(self.error("expected '=' after an attribute name"));
        }
        self.pos += 1;
        self.skip_whitespace();
        let quote = match self.input.get(self.pos) {
            Some(&q @ (b'"' | b'\'')) => q,
            _ => return Err(self.error("attribute values must be quoted")),
        };
        self.pos += 1;
        let value_start = self.pos;
        let len = memchr(quote, &self.input[value_start..])
            .ok_or_else(|| self.error("unterminated attribute value"))?;
        let value = &self.input[value_start..value_start + len];
        if memchr(b'<', value).is_some() {
            return Err(self.error("'<' is not allowed in attribute values"));
        }
        validate_references(value, value_start)?;
        self.pos = value_start + len + 1;
        Ok(Attribute {
            raw: self.input[raw_start..self.pos].to_vec(),
            name,
            value: value_start - raw_start..value_start - raw_start + len,
            quote,
        })
    }

    fn end_tag(&mut self, current: usize) -> Result<Vec<u8>, XmlError> {
        let start = self.pos;
        self.pos += 2;
        let name = self.name()?;
        self.skip_whitespace();
        if self.input.get(self.pos) != Some(&b'>') {
            return Err(self.error("expected '>' to close the end tag"));
        }
        let matches = matches!(self.nodes.get(current), Some(Node::Element(e)) if e.name == name);
        if !matches {
            self.pos = start;
            return Err(self.error("end tag does not match the open element"));
        }
        self.pos += 1;
        Ok(self.input[start..self.pos].to_vec())
    }

    fn text(&mut self) -> Result<usize, XmlError> {
        let start = self.pos;
        let len = memchr(b'<', &self.input[start..]).unwrap_or(self.input.len() - start);
        let raw = &self.input[start..start + len];
        validate_references(raw, start)?;
        self.pos = start + len;
        Ok(self.push(Node::Text(raw.to_vec())))
    }
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80
}

fn is_name_char(b: u8) -> bool {
    is_name_start(b) || b.is_ascii_digit() || b == b'-' || b == b'.'
}

fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.first().is_some_and(|&b| is_name_start(b))
        && bytes.iter().all(|&b| is_name_char(b))
        && bytes.len() <= MAX_NAME_LEN
}

/// Characters allowed in XML 1.0 documents.
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// Extracts the `encoding` pseudo-attribute of an XML declaration.
fn declared_encoding(declaration: &[u8]) -> Option<&[u8]> {
    let at = memmem::find(declaration, b"encoding")?;
    let rest = &declaration[at + 8..];
    let rest = &rest[rest.iter().position(|&b| !is_whitespace(b))?..];
    let rest = rest.strip_prefix(b"=")?;
    let rest = &rest[rest.iter().position(|&b| !is_whitespace(b))?..];
    let (&quote, rest) = rest.split_first()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    Some(&rest[..memchr(quote, rest)?])
}

/// Accepts only the predefined entities and character references.
fn validate_references(bytes: &[u8], offset: usize) -> Result<(), XmlError> {
    let mut from = 0;
    while let Some(at) = memchr(b'&', &bytes[from..]) {
        let start = from + at + 1;
        let window = &bytes[start..bytes.len().min(start + 32)];
        let len = memchr(b';', window).ok_or(XmlError::Syntax {
            offset: offset + start - 1,
            reason: "unterminated reference",
        })?;
        let name = &bytes[start..start + len];
        if reference_char(name).is_none() {
            return Err(XmlError::Entity(String::from_utf8_lossy(name).into_owned()));
        }
        from = start + len + 1;
    }
    Ok(())
}

/// The character a reference name (without `&` and `;`) stands for.
fn reference_char(name: &[u8]) -> Option<char> {
    match name {
        b"amp" => Some('&'),
        b"lt" => Some('<'),
        b"gt" => Some('>'),
        b"quot" => Some('"'),
        b"apos" => Some('\''),
        [b'#', b'x', hex @ ..]
            if !hex.is_empty() && hex.len() <= 6 && hex.iter().all(u8::is_ascii_hexdigit) =>
        {
            let code = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            char::from_u32(code).filter(|&c| is_xml_char(c))
        }
        [b'#', digits @ ..]
            if !digits.is_empty() && digits.len() <= 7 && digits.iter().all(u8::is_ascii_digit) =>
        {
            let code = std::str::from_utf8(digits).ok()?.parse().ok()?;
            char::from_u32(code).filter(|&c| is_xml_char(c))
        }
        _ => None,
    }
}

/// Replaces references with the characters they stand for. Unknown
/// references are kept (the parser has already rejected them).
fn resolve_references(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        match after
            .find(';')
            .and_then(|end| Some((reference_char(&after.as_bytes()[..end])?, end)))
        {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Attribute-value normalization for literal whitespace characters.
fn normalize_attribute(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

fn escape_attribute(value: &str, quote: u8) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' if quote == b'"' => out.push_str("&quot;"),
            '\'' if quote == b'\'' => out.push_str("&apos;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!-- lab order -->\n\
<ns:order xmlns:ns=\"urn:example\" id='A&amp;1'>\n  \
<test code=\"GLU\">Glucose</test>\n  \
<test code=\"HGB\" priority = \"stat\" >Hemo&#x67;lobin <![CDATA[<b>]]></test>\n  \
<empty />\n\
</ns:order>\n";

    fn parse(text: &str) -> XmlDocument {
        XmlDocument::parse(text.as_bytes(), &XmlOptions::default()).unwrap()
    }

    #[test]
    fn keeps_unmodified_documents_byte_for_byte() {
        assert_eq!(parse(ORDER).to_bytes(), ORDER.as_bytes());
    }

    #[test]
    fn walks_elements() {
        let doc = parse(ORDER);
        let root = doc.root();
        assert_eq!(
            (root.name(), root.local_name()),
            ("ns:order".into(), "order".into())
        );
        assert!(root.is("order") && root.is("ns:order") && !root.is("x:order"));
        assert_eq!(root.attribute("id").as_deref(), Some("A&1"));
        assert_eq!(root.children().count(), 3);
        let tests: Vec<_> = root.children_named("test").collect();
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[1].attribute("priority").as_deref(), Some("stat"));
        assert_eq!(tests[1].text(), "Hemoglobin <b>");
        assert!(root.child("missing").is_none());
        assert_eq!(
            root.text_content().split_whitespace().collect::<Vec<_>>(),
            ["Glucose", "Hemoglobin", "<b>"]
        );
        let second = doc.element_at("/order/test[2]").unwrap().unwrap();
        assert_eq!(second.attribute("code").as_deref(), Some("HGB"));
        assert!(doc.element_at("/order/test[3]").unwrap().is_none());
        assert!(doc.element_at("/order/test/@code").is_err());
        assert!(root.children_named("bad name").next().is_none());
    }

    #[test]
    fn reads_paths() {
        let doc = parse(ORDER);
        assert_eq!(doc.root_name(), "ns:order");
        assert_eq!(doc.get("/order/@id").unwrap().as_deref(), Some("A&1"));
        assert_eq!(
            doc.get("/ns:order/test[2]/@code").unwrap().as_deref(),
            Some("HGB")
        );
        assert_eq!(
            doc.get("/order/test[2]").unwrap().as_deref(),
            Some("Hemoglobin <b>")
        );
        assert_eq!(
            doc.get("/order/test/text()").unwrap().as_deref(),
            Some("Glucose")
        );
        assert_eq!(doc.get("/order/*[3]").unwrap().as_deref(), Some(""));
        assert_eq!(doc.get("/order/test[3]").unwrap(), None);
        assert_eq!(doc.get("/other/test").unwrap(), None);
        assert_eq!(doc.get("/x:order/test").unwrap(), None);
        assert_eq!(doc.count("/order/test").unwrap(), 2);
        for invalid in [
            "order",
            "/",
            "//order",
            "/order/test[0]",
            "/order/@*",
            "/order/te st",
            "/order/test[x]",
        ] {
            assert!(invalid.parse::<XmlPath>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn edits_only_what_they_touch() {
        let mut doc = parse(ORDER);
        doc.set("/order/test[1]/@code", "GLU2").unwrap();
        doc.set("/order/test[2]", "a < b & c").unwrap();
        doc.set("/order/@id", "B'2").unwrap();
        let expected = ORDER
            .replace("code=\"GLU\"", "code=\"GLU2\"")
            .replace("Hemo&#x67;lobin <![CDATA[<b>]]>", "a &lt; b &amp; c")
            .replace("id='A&amp;1'", "id='B&apos;2'");
        assert_eq!(String::from_utf8(doc.to_bytes()).unwrap(), expected);
        let reparsed = XmlDocument::parse(&doc.to_bytes(), &XmlOptions::default()).unwrap();
        assert_eq!(
            reparsed.get("/order/test[2]").unwrap().as_deref(),
            Some("a < b & c")
        );
        assert_eq!(reparsed.get("/order/@id").unwrap().as_deref(), Some("B'2"));
    }

    #[test]
    fn creates_elements_and_attributes() {
        let mut doc = parse("<order/>");
        doc.set("/order/test[1]/@code", "GLU").unwrap();
        doc.set("/order/test[2]", "HGB").unwrap();
        doc.set("/order/empty", "").unwrap();
        assert_eq!(
            String::from_utf8(doc.to_bytes()).unwrap(),
            "<order><test code=\"GLU\"/><test>HGB</test><empty/></order>"
        );
        assert!(matches!(
            doc.set("/order/test[5]", "x"),
            Err(XmlError::NotFound(_))
        ));
        assert!(matches!(
            doc.set("/other/test", "x"),
            Err(XmlError::NotFound(_))
        ));
        assert_eq!(
            doc.set("/order/test", "\u{1}"),
            Err(XmlError::InvalidCharacter)
        );
    }

    type Check = fn(&XmlError) -> bool;

    #[test]
    fn rejects_dangerous_or_malformed_documents() {
        let cases: &[(&str, Check)] = &[
            ("<!DOCTYPE a [<!ENTITY x \"y\">]><a/>", |e| {
                matches!(e, XmlError::DocumentType(_))
            }),
            ("<a>&x;</a>", |e| matches!(e, XmlError::Entity(_))),
            ("<a b='&ext;'/>", |e| matches!(e, XmlError::Entity(_))),
            ("<a>&#0;</a>", |e| matches!(e, XmlError::Entity(_))),
            ("<a>&amp</a>", |e| matches!(e, XmlError::Syntax { .. })),
            ("<a></b>", |e| matches!(e, XmlError::Syntax { .. })),
            ("<a>", |e| matches!(e, XmlError::Syntax { .. })),
            ("<a b='1' b='2'/>", |e| matches!(e, XmlError::Syntax { .. })),
            ("<a b=1/>", |e| matches!(e, XmlError::Syntax { .. })),
            ("<a/><b/>", |e| matches!(e, XmlError::Syntax { .. })),
            ("text<a/>", |e| matches!(e, XmlError::Syntax { .. })),
            ("<a><?xml version='1.0'?></a>", |e| {
                matches!(e, XmlError::Syntax { .. })
            }),
            ("<?xml version='1.0' encoding='shift_jis'?><a/>", |e| {
                matches!(e, XmlError::UnsupportedEncoding(_))
            }),
            ("", |e| matches!(e, XmlError::Syntax { .. })),
        ];
        for (input, check) in cases {
            let error = XmlDocument::parse(input.as_bytes(), &XmlOptions::default()).unwrap_err();
            assert!(check(&error), "{input:?} gave {error:?}");
        }
    }

    #[test]
    fn enforces_limits() {
        let options = XmlOptions {
            max_depth: 2,
            ..XmlOptions::default()
        };
        assert!(XmlDocument::parse(b"<a><b/></a>", &options).is_ok());
        assert_eq!(
            XmlDocument::parse(b"<a><b><c/></b></a>", &options),
            Err(XmlError::LimitExceeded("depth"))
        );
        let options = XmlOptions {
            max_attributes: 1,
            ..XmlOptions::default()
        };
        assert_eq!(
            XmlDocument::parse(b"<a x='1' y='2'/>", &options),
            Err(XmlError::LimitExceeded("attributes per element"))
        );
    }

    #[test]
    fn decodes_declared_encodings() {
        let mut input = b"<?xml version='1.0' encoding='ISO-8859-9'?><hasta ad='".to_vec();
        input.extend_from_slice(b"\xDEahin'/>");
        let mut doc = XmlDocument::parse(&input, &XmlOptions::default()).unwrap();
        assert_eq!(doc.get("/hasta/@ad").unwrap().as_deref(), Some("Şahin"));
        doc.set("/hasta/@not", "ölçüm 😀").unwrap();
        let bytes = doc.to_bytes();
        assert!(bytes.ends_with(b" not=\"\xF6l\xE7\xFCm &#128512;\"/>"));
        let reparsed = XmlDocument::parse(&bytes, &XmlOptions::default()).unwrap();
        assert_eq!(
            reparsed.get("/hasta/@not").unwrap().as_deref(),
            Some("ölçüm 😀")
        );
    }

    #[test]
    fn normalizes_attribute_whitespace() {
        let doc = parse("<a v='x\ny&#10;z'/>");
        assert_eq!(doc.get("/a/@v").unwrap().as_deref(), Some("x y\nz"));
    }
}
