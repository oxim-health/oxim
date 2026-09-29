//! Splits a byte stream into consecutive XML documents.
//!
//! POCT1-A devices send one XML document per message over the same
//! connection, optionally preceded by an XML declaration and separated by
//! whitespace. The splitter finds document boundaries without building a
//! tree: it tokenizes markup just enough to track element depth, quoted
//! attribute values, comments, CDATA sections and processing instructions.
//! Complete documents are then parsed with [`Message::parse`](crate::Message::parse).

use memchr::{memchr, memmem};

/// Options for [`Splitter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SplitterOptions {
    /// The largest document accepted, in bytes. Larger documents are
    /// dropped without being buffered completely.
    pub max_document_len: usize,
    /// The deepest element nesting accepted.
    pub max_depth: usize,
}

impl Default for SplitterOptions {
    fn default() -> Self {
        Self {
            max_document_len: 4 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

/// Why bytes were dropped by the [`Splitter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiscardReason {
    /// Non-whitespace bytes outside a document.
    OutsideDocument,
    /// A document exceeded [`SplitterOptions::max_document_len`].
    TooLarge,
    /// A document exceeded [`SplitterOptions::max_depth`].
    TooDeep,
    /// A document contained a document type or other markup declaration,
    /// which is rejected for security reasons.
    Forbidden,
    /// Markup that cannot belong to a well-formed document, such as text or
    /// a closing tag before the root element, or an XML declaration that
    /// starts a new document before the previous one had a root element.
    Malformed,
    /// The stream ended inside a document.
    Truncated,
}

/// Something the splitter found in the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitEvent {
    /// The bytes of one complete document, from its first markup (XML
    /// declaration, comment or root element) to the end of the root element.
    Document(Vec<u8>),
    /// Bytes that were dropped; splitting continues.
    Discarded {
        /// How many bytes were dropped.
        bytes: usize,
        /// Why they were dropped.
        reason: DiscardReason,
    },
}

/// Incremental splitter of a byte stream into XML documents.
///
/// Push bytes with [`Splitter::push`], then call [`Splitter::next_event`]
/// until it returns `None`. Call [`Splitter::finish`] when the stream ends.
/// Whitespace between documents is dropped silently.
#[derive(Debug, Clone)]
pub struct Splitter {
    options: SplitterOptions,
    buffer: Vec<u8>,
    document: Option<Document>,
}

/// State of the document being split. `buffer[..pos]` has been tokenized.
#[derive(Debug, Clone)]
struct Document {
    pos: usize,
    depth: usize,
    root_seen: bool,
    problem: Option<DiscardReason>,
    /// Bytes of this document already removed from the buffer because the
    /// document will be discarded anyway.
    dropped: usize,
    partial: Option<Partial>,
}

/// Resume state for a markup token that is not complete yet. Offsets are
/// relative to the start of the token.
#[derive(Debug, Clone, Copy)]
struct Partial {
    kind: Markup,
    scanned: usize,
    quote: Option<u8>,
    brackets: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Markup {
    StartTag,
    EndTag,
    Comment,
    CData,
    ProcessingInstruction {
        declaration: bool,
    },
    /// `<!DOCTYPE ...>` or another `<!...>` markup declaration.
    Declaration,
}

enum Scan {
    NeedMore,
    Text(usize),
    Markup {
        kind: Markup,
        len: usize,
        self_closing: bool,
    },
}

impl Default for Splitter {
    fn default() -> Self {
        Self::new(SplitterOptions::default())
    }
}

impl Splitter {
    /// Creates a splitter.
    pub fn new(options: SplitterOptions) -> Self {
        Self {
            options,
            buffer: Vec::new(),
            document: None,
        }
    }

    /// Appends received bytes.
    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// The number of buffered bytes not yet returned as an event.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    /// Whether the splitter is inside a document.
    pub fn in_document(&self) -> bool {
        self.document.is_some()
    }

    /// Returns the next event, or `None` when more bytes are needed.
    pub fn next_event(&mut self) -> Option<SplitEvent> {
        loop {
            if self.document.is_none() {
                if let Some(event) = self.between_documents() {
                    return Some(event);
                }
                // Still outside a document: more bytes are needed.
                self.document.as_ref()?;
            }
            if let Some(result) = self.step() {
                return result;
            }
        }
    }

    /// Signals the end of the stream and resets the splitter. Returns a
    /// [`DiscardReason::Truncated`] event for an incomplete document, or a
    /// [`DiscardReason::OutsideDocument`] event for leftover non-whitespace
    /// bytes. Call [`Splitter::next_event`] until it returns `None` first.
    pub fn finish(&mut self) -> Option<SplitEvent> {
        let event = match self.document.take() {
            Some(document) => Some(SplitEvent::Discarded {
                bytes: document.dropped + self.buffer.len(),
                reason: DiscardReason::Truncated,
            }),
            None if self.buffer.iter().any(|b| !is_whitespace(*b)) => Some(SplitEvent::Discarded {
                bytes: self.buffer.len(),
                reason: DiscardReason::OutsideDocument,
            }),
            None => None,
        };
        self.buffer.clear();
        event
    }

    /// Skips whitespace and junk until a document starts. Returns an event
    /// for discarded junk; otherwise either starts a document or needs more
    /// bytes.
    fn between_documents(&mut self) -> Option<SplitEvent> {
        let whitespace = self
            .buffer
            .iter()
            .position(|b| !is_whitespace(*b))
            .unwrap_or(self.buffer.len());
        self.buffer.drain(..whitespace);
        let first = *self.buffer.first()?;
        if first == b'<' {
            let second = *self.buffer.get(1)?;
            if matches!(second, b'?' | b'!' | b'/') || is_name_start(second) {
                self.document = Some(Document {
                    pos: 0,
                    depth: 0,
                    root_seen: false,
                    problem: None,
                    dropped: 0,
                    partial: None,
                });
                return None;
            }
            return Some(self.discard(1, DiscardReason::OutsideDocument));
        }
        let junk = memchr(b'<', &self.buffer).unwrap_or(self.buffer.len());
        Some(self.discard(junk, DiscardReason::OutsideDocument))
    }

    /// Processes one token of the current document. Returns `Some(event)`
    /// or `Some(None)` to stop, and `None` to continue the loop.
    fn step(&mut self) -> Option<Option<SplitEvent>> {
        let max = self.options.max_document_len;
        let document = self.document.as_mut()?;
        match scan(&self.buffer[document.pos..], &mut document.partial) {
            Scan::NeedMore => {
                if self.buffer.len() - document.pos > max {
                    // A single token is larger than a whole document may be:
                    // give up on this document and resynchronize.
                    let bytes = document.dropped + self.buffer.len();
                    self.buffer.clear();
                    self.document = None;
                    return Some(Some(SplitEvent::Discarded {
                        bytes,
                        reason: DiscardReason::TooLarge,
                    }));
                }
                self.drop_consumed_if_doomed();
                Some(None)
            }
            Scan::Text(len) => {
                let text = &self.buffer[document.pos..document.pos + len];
                if document.depth == 0 && text.iter().any(|b| !is_whitespace(*b)) {
                    document.problem.get_or_insert(DiscardReason::Malformed);
                }
                document.pos += len;
                self.drop_consumed_if_doomed();
                None
            }
            Scan::Markup {
                kind,
                len,
                self_closing,
            } => {
                let at_start = document.pos == 0 && document.dropped == 0;
                if kind == (Markup::ProcessingInstruction { declaration: true })
                    && !at_start
                    && document.depth == 0
                {
                    // An XML declaration starts the next document: what came
                    // before it never reached a root element.
                    let bytes = document.dropped + document.pos;
                    let pos = document.pos;
                    self.buffer.drain(..pos);
                    *document = Document {
                        pos: 0,
                        depth: 0,
                        root_seen: false,
                        problem: None,
                        dropped: 0,
                        partial: None,
                    };
                    return Some(Some(SplitEvent::Discarded {
                        bytes,
                        reason: DiscardReason::Malformed,
                    }));
                }
                document.pos += len;
                let complete = match kind {
                    Markup::StartTag => {
                        document.root_seen = true;
                        if self_closing {
                            document.depth == 0
                        } else {
                            document.depth += 1;
                            if document.depth > self.options.max_depth {
                                document.problem.get_or_insert(DiscardReason::TooDeep);
                            }
                            false
                        }
                    }
                    Markup::EndTag => match document.depth {
                        0 => {
                            document.problem.get_or_insert(DiscardReason::Malformed);
                            true
                        }
                        depth => {
                            document.depth = depth - 1;
                            document.depth == 0
                        }
                    },
                    Markup::CData if document.depth == 0 => {
                        document.problem.get_or_insert(DiscardReason::Malformed);
                        false
                    }
                    Markup::ProcessingInstruction { declaration: true } if document.depth > 0 => {
                        document.problem.get_or_insert(DiscardReason::Malformed);
                        false
                    }
                    Markup::Declaration => {
                        document.problem.get_or_insert(DiscardReason::Forbidden);
                        false
                    }
                    Markup::Comment | Markup::CData | Markup::ProcessingInstruction { .. } => false,
                };
                if complete {
                    return Some(Some(self.complete()));
                }
                if document.dropped + document.pos > max {
                    document.problem.get_or_insert(DiscardReason::TooLarge);
                }
                self.drop_consumed_if_doomed();
                None
            }
        }
    }

    /// Emits the finished document, or discards it if it had a problem.
    fn complete(&mut self) -> SplitEvent {
        let Some(document) = self.document.take() else {
            return SplitEvent::Discarded {
                bytes: 0,
                reason: DiscardReason::Malformed,
            };
        };
        let bytes: Vec<u8> = self.buffer.drain(..document.pos).collect();
        let too_large = document.dropped + bytes.len() > self.options.max_document_len;
        match document.problem {
            None if !too_large => SplitEvent::Document(bytes),
            problem => SplitEvent::Discarded {
                bytes: document.dropped + bytes.len(),
                reason: problem.unwrap_or(DiscardReason::TooLarge),
            },
        }
    }

    /// Frees the tokenized bytes of a document that will be discarded.
    fn drop_consumed_if_doomed(&mut self) {
        let max = self.options.max_document_len;
        if let Some(document) = self.document.as_mut() {
            if document.dropped + document.pos > max {
                document.problem.get_or_insert(DiscardReason::TooLarge);
            }
            if document.problem.is_some() && document.pos > 0 {
                self.buffer.drain(..document.pos);
                document.dropped += document.pos;
                document.pos = 0;
            }
        }
    }

    fn discard(&mut self, len: usize, reason: DiscardReason) -> SplitEvent {
        self.buffer.drain(..len);
        SplitEvent::Discarded { bytes: len, reason }
    }
}

/// Scans the token at the start of `rest`.
fn scan(rest: &[u8], partial: &mut Option<Partial>) -> Scan {
    let Some(&first) = rest.first() else {
        return Scan::NeedMore;
    };
    if partial.is_none() && first != b'<' {
        return Scan::Text(memchr(b'<', rest).unwrap_or(rest.len()));
    }
    let mut state = match *partial {
        Some(state) => state,
        None => {
            let Some(kind) = classify(rest) else {
                return Scan::NeedMore;
            };
            let scanned = match kind {
                Markup::StartTag => 1,
                Markup::EndTag | Markup::ProcessingInstruction { .. } | Markup::Declaration => 2,
                Markup::Comment => 4,
                Markup::CData => 9,
            };
            Partial {
                kind,
                scanned,
                quote: None,
                brackets: 0,
            }
        }
    };
    match find_end(rest, &mut state) {
        Some((len, self_closing)) => {
            *partial = None;
            Scan::Markup {
                kind: state.kind,
                len,
                self_closing,
            }
        }
        None => {
            *partial = Some(state);
            Scan::NeedMore
        }
    }
}

/// Classifies markup starting with `<`, or returns `None` when more bytes
/// are needed to decide.
fn classify(bytes: &[u8]) -> Option<Markup> {
    match *bytes.get(1)? {
        b'/' => Some(Markup::EndTag),
        b'?' => {
            const XML: &[u8] = b"<?xml";
            if bytes.len() <= XML.len() && XML.starts_with(bytes) {
                return None;
            }
            let declaration = bytes.starts_with(XML)
                && bytes
                    .get(XML.len())
                    .is_some_and(|&b| is_whitespace(b) || b == b'?');
            Some(Markup::ProcessingInstruction { declaration })
        }
        b'!' => {
            for (prefix, kind) in [
                (&b"<!--"[..], Markup::Comment),
                (b"<![CDATA[", Markup::CData),
            ] {
                if bytes.starts_with(prefix) {
                    return Some(kind);
                }
                if prefix.starts_with(bytes) {
                    return None;
                }
            }
            Some(Markup::Declaration)
        }
        _ => Some(Markup::StartTag),
    }
}

/// Finds the end of the markup token at the start of `rest`, returning its
/// length and whether it is a self-closing tag.
fn find_end(rest: &[u8], state: &mut Partial) -> Option<(usize, bool)> {
    let terminator: &[u8] = match state.kind {
        Markup::Comment => b"-->",
        Markup::CData => b"]]>",
        Markup::ProcessingInstruction { .. } => b"?>",
        Markup::StartTag | Markup::EndTag | Markup::Declaration => {
            for (i, &b) in rest.iter().enumerate().skip(state.scanned) {
                match state.quote {
                    Some(quote) if b == quote => state.quote = None,
                    Some(_) => {}
                    None => match b {
                        b'"' | b'\'' => state.quote = Some(b),
                        b'[' if state.kind == Markup::Declaration => state.brackets += 1,
                        b']' if state.kind == Markup::Declaration => {
                            state.brackets = state.brackets.saturating_sub(1);
                        }
                        b'>' if state.brackets == 0 => {
                            let self_closing =
                                state.kind == Markup::StartTag && i > 1 && rest[i - 1] == b'/';
                            return Some((i + 1, self_closing));
                        }
                        _ => {}
                    },
                }
            }
            state.scanned = rest.len();
            return None;
        }
    };
    let minimum = match state.kind {
        Markup::Comment => 4,
        Markup::CData => 9,
        _ => 2,
    };
    let from = state
        .scanned
        .saturating_sub(terminator.len() - 1)
        .max(minimum);
    match rest
        .get(from..)
        .and_then(|tail| memmem::find(tail, terminator))
    {
        Some(at) => Some((from + at + terminator.len(), false)),
        None => {
            state.scanned = rest.len();
            None
        }
    }
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// Whether `b` can start an element name: an ASCII letter, `_`, `:` or the
/// first byte of a non-ASCII UTF-8 character.
fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<SplitEvent> {
        split_with(SplitterOptions::default(), chunks)
    }

    fn split_with(options: SplitterOptions, chunks: &[&[u8]]) -> Vec<SplitEvent> {
        let mut splitter = Splitter::new(options);
        let mut events = Vec::new();
        for chunk in chunks {
            splitter.push(chunk);
            events.extend(std::iter::from_fn(|| splitter.next_event()));
        }
        events.extend(splitter.finish());
        events
    }

    fn document(bytes: &[u8]) -> SplitEvent {
        SplitEvent::Document(bytes.to_vec())
    }

    fn discarded(bytes: usize, reason: DiscardReason) -> SplitEvent {
        SplitEvent::Discarded { bytes, reason }
    }

    #[test]
    fn splits_consecutive_documents() {
        let stream = b"<?xml version=\"1.0\"?>\r\n<HEL.R01><HDR/></HEL.R01>\n\n<DST.R01 a='>'/>  <!-- c --><ACK.R01><![CDATA[</ACK.R01>]]><?pi x?></ACK.R01>";
        assert_eq!(
            split(&[stream]),
            [
                document(b"<?xml version=\"1.0\"?>\r\n<HEL.R01><HDR/></HEL.R01>"),
                document(b"<DST.R01 a='>'/>"),
                document(b"<!-- c --><ACK.R01><![CDATA[</ACK.R01>]]><?pi x?></ACK.R01>"),
            ]
        );
    }

    #[test]
    fn survives_byte_by_byte_delivery() {
        let stream: &[u8] =
            b"<?xml version='1.0'?><A b=\"x>y\"><!-- a > b --><B/><![CDATA[ ]]> ]]></A> <C/>";
        let chunks: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(
            split(&chunks),
            [
                document(
                    b"<?xml version='1.0'?><A b=\"x>y\"><!-- a > b --><B/><![CDATA[ ]]> ]]></A>"
                ),
                document(b"<C/>"),
            ]
        );
    }

    #[test]
    fn reports_junk_between_documents() {
        assert_eq!(
            split(&[b"garbage<A/>\0\0<B/>< <C/>"]),
            [
                discarded(7, DiscardReason::OutsideDocument),
                document(b"<A/>"),
                discarded(2, DiscardReason::OutsideDocument),
                document(b"<B/>"),
                discarded(1, DiscardReason::OutsideDocument),
                document(b"<C/>"),
            ]
        );
    }

    #[test]
    fn rejects_forbidden_and_malformed_documents() {
        assert_eq!(
            split(&[b"<!DOCTYPE a [<!ENTITY x '>'>]><a>&x;</a><B/>"]),
            [discarded(40, DiscardReason::Forbidden), document(b"<B/>")]
        );
        assert_eq!(
            split(&[b"</stray><B/>"]),
            [discarded(8, DiscardReason::Malformed), document(b"<B/>")]
        );
        assert_eq!(
            split(&[b"<?xml version='1.0'?>text<A/><?xml version='1.0'?><B/>"]),
            [
                discarded(29, DiscardReason::Malformed),
                document(b"<?xml version='1.0'?><B/>")
            ]
        );
        assert_eq!(
            split(&[b"<?xml version='1.0'?><?xml version='1.0'?><B/>"]),
            [
                discarded(21, DiscardReason::Malformed),
                document(b"<?xml version='1.0'?><B/>")
            ]
        );
    }

    #[test]
    fn limits_size_and_depth() {
        let options = SplitterOptions {
            max_document_len: 16,
            ..SplitterOptions::default()
        };
        assert_eq!(
            split_with(options, &[b"<A>", b"0123456789", b"0123456789</A><B/>"]),
            [discarded(27, DiscardReason::TooLarge), document(b"<B/>")]
        );
        assert_eq!(
            split_with(options, &[b"<A b='", &[b'x'; 40], b"'/><B/>"]),
            [
                discarded(46, DiscardReason::TooLarge),
                discarded(3, DiscardReason::OutsideDocument),
                document(b"<B/>")
            ]
        );
        let options = SplitterOptions {
            max_depth: 2,
            ..SplitterOptions::default()
        };
        assert_eq!(
            split_with(options, &[b"<a><b><c/></b></a><a><b><c></c></b></a><D/>"]),
            [
                document(b"<a><b><c/></b></a>"),
                discarded(21, DiscardReason::TooDeep),
                document(b"<D/>")
            ]
        );
    }

    #[test]
    fn reports_truncated_streams() {
        assert_eq!(
            split(&[b"<A><B>"]),
            [discarded(6, DiscardReason::Truncated)]
        );
        assert_eq!(split(&[b"  \r\n"]), []);
        assert_eq!(
            split(&[b"<"]),
            [discarded(1, DiscardReason::OutsideDocument)]
        );
    }
}
