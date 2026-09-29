//! ASTM E1394 messages sent without LIS01 framing.
//!
//! Many analyzers connected over TCP send records separated by CR without
//! the LIS01 ENQ/STX/ACK handshake. [`RawSplitter`] cuts such a byte stream
//! into messages, from a header (`H`) record through a terminator (`L`)
//! record, keeping every byte of the records it returns.

use memchr::memchr2;

/// Options for [`RawSplitter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RawOptions {
    /// The largest message accepted. Larger messages are discarded without
    /// buffering them completely.
    pub max_message_len: usize,
}

impl Default for RawOptions {
    fn default() -> Self {
        Self {
            max_message_len: 16 * 1024 * 1024,
        }
    }
}

/// Why the splitter dropped bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RawDiscardReason {
    /// Records outside a message (before a header record).
    OutsideMessage,
    /// A new header record arrived before the terminator record; the
    /// unfinished message was dropped.
    Interrupted,
    /// A message or line exceeded [`RawOptions::max_message_len`].
    TooLarge,
    /// The stream ended inside a message.
    Truncated,
}

/// Something the splitter found in the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawEvent {
    /// A complete message, header through terminator record, including the
    /// original record terminators.
    Message(Vec<u8>),
    /// Bytes that were dropped; the stream continues.
    Discarded {
        /// How many bytes.
        bytes: usize,
        /// Why.
        reason: RawDiscardReason,
    },
}

/// Incremental splitter for unframed ASTM E1394 streams.
///
/// Records end with CR, LF or CRLF. A CR that is the last buffered byte ends
/// its record immediately, so a following LF, if it arrives later, is
/// reported as one byte outside a message.
#[derive(Debug, Clone)]
pub struct RawSplitter {
    options: RawOptions,
    buffer: Vec<u8>,
    message: Vec<u8>,
    /// The field delimiter of the message being collected, if any.
    field_delimiter: Option<u8>,
    /// Bytes dropped from an oversized message, while skipping to its end.
    skipped: Option<usize>,
}

impl Default for RawSplitter {
    fn default() -> Self {
        Self::new(RawOptions::default())
    }
}

impl RawSplitter {
    /// Creates a splitter.
    pub fn new(options: RawOptions) -> Self {
        Self {
            options,
            buffer: Vec::new(),
            message: Vec::new(),
            field_delimiter: None,
            skipped: None,
        }
    }

    /// Appends received bytes.
    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Whether the splitter is inside a message.
    pub fn in_message(&self) -> bool {
        self.field_delimiter.is_some()
    }

    /// Returns the next event, or `None` when more bytes are needed.
    pub fn next_event(&mut self) -> Option<RawEvent> {
        loop {
            let Some(at) = memchr2(b'\r', b'\n', &self.buffer) else {
                return self.check_partial_line();
            };
            let len = if self.buffer[at] == b'\r' && self.buffer.get(at + 1) == Some(&b'\n') {
                at + 2
            } else {
                at + 1
            };
            let line: Vec<u8> = self.buffer.drain(..len).collect();
            if let Some(event) = self.on_line(line) {
                return Some(event);
            }
        }
    }

    /// Signals the end of the stream and resets the splitter. A final
    /// terminator record without a line ending still completes its message.
    pub fn finish(&mut self) -> Option<RawEvent> {
        let buffer = std::mem::take(&mut self.buffer);
        let message = std::mem::take(&mut self.message);
        let delimiter = self.field_delimiter.take();
        let skipped = self.skipped.take();
        if let Some(skipped) = skipped {
            return Some(RawEvent::Discarded {
                bytes: skipped + buffer.len(),
                reason: RawDiscardReason::TooLarge,
            });
        }
        match delimiter {
            Some(delimiter) if is_terminator(&buffer, delimiter) => {
                let mut message = message;
                message.extend_from_slice(&buffer);
                Some(RawEvent::Message(message))
            }
            Some(_) => Some(RawEvent::Discarded {
                bytes: message.len() + buffer.len(),
                reason: RawDiscardReason::Truncated,
            }),
            None if buffer.is_empty() => None,
            None => Some(RawEvent::Discarded {
                bytes: buffer.len(),
                // An unterminated header line started a message that never
                // ended.
                reason: if header_delimiter(&buffer).is_some() {
                    RawDiscardReason::Truncated
                } else {
                    RawDiscardReason::OutsideMessage
                },
            }),
        }
    }

    fn on_line(&mut self, line: Vec<u8>) -> Option<RawEvent> {
        let max = self.options.max_message_len;
        if let Some(delimiter) = header_delimiter(&line) {
            let event = if let Some(skipped) = self.skipped.take() {
                Some(RawEvent::Discarded {
                    bytes: skipped,
                    reason: RawDiscardReason::TooLarge,
                })
            } else if !self.message.is_empty() {
                Some(RawEvent::Discarded {
                    bytes: self.message.len(),
                    reason: RawDiscardReason::Interrupted,
                })
            } else {
                None
            };
            self.field_delimiter = Some(delimiter);
            if line.len() > max {
                self.message.clear();
                self.skipped = Some(line.len());
            } else {
                self.message = line;
            }
            return event;
        }
        let Some(delimiter) = self.field_delimiter else {
            return Some(RawEvent::Discarded {
                bytes: line.len(),
                reason: RawDiscardReason::OutsideMessage,
            });
        };
        let terminator = is_terminator(&line, delimiter);
        if let Some(skipped) = self.skipped.as_mut() {
            *skipped += line.len();
            if !terminator {
                return None;
            }
            let bytes = *skipped;
            self.skipped = None;
            self.field_delimiter = None;
            return Some(RawEvent::Discarded {
                bytes,
                reason: RawDiscardReason::TooLarge,
            });
        }
        if self.message.len() + line.len() > max {
            let bytes = self.message.len() + line.len();
            self.message.clear();
            if terminator {
                self.field_delimiter = None;
                return Some(RawEvent::Discarded {
                    bytes,
                    reason: RawDiscardReason::TooLarge,
                });
            }
            self.skipped = Some(bytes);
            return None;
        }
        self.message.extend_from_slice(&line);
        if terminator {
            self.field_delimiter = None;
            return Some(RawEvent::Message(std::mem::take(&mut self.message)));
        }
        None
    }

    /// Bounds a line that has not ended yet.
    fn check_partial_line(&mut self) -> Option<RawEvent> {
        if self.buffer.len() <= self.options.max_message_len {
            return None;
        }
        let bytes = self.buffer.len();
        self.buffer.clear();
        if self.field_delimiter.is_some() {
            let previous = self.skipped.unwrap_or(self.message.len());
            self.message.clear();
            self.skipped = Some(previous + bytes);
            return None;
        }
        Some(RawEvent::Discarded {
            bytes,
            reason: RawDiscardReason::TooLarge,
        })
    }
}

/// The field delimiter if `line` is a header record: `H` followed by a
/// printable, non-alphanumeric delimiter.
fn header_delimiter(line: &[u8]) -> Option<u8> {
    match line {
        [b'H', delimiter, ..]
            if delimiter.is_ascii_graphic() && !delimiter.is_ascii_alphanumeric() =>
        {
            Some(*delimiter)
        }
        _ => None,
    }
}

/// Whether `line` is a terminator record: `L`, alone or followed by the field
/// delimiter.
fn is_terminator(line: &[u8], delimiter: u8) -> bool {
    let content = line.trim_ascii_end();
    content == b"L" || (content.first() == Some(&b'L') && content.get(1) == Some(&delimiter))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(splitter: &mut RawSplitter) -> Vec<RawEvent> {
        std::iter::from_fn(|| splitter.next_event()).collect()
    }

    fn message(bytes: &[u8]) -> RawEvent {
        RawEvent::Message(bytes.to_vec())
    }

    #[test]
    fn splits_messages_and_reports_noise() {
        let mut splitter = RawSplitter::default();
        splitter.push(b"noise\rH|\\^&\rR|1|^^^GLU|5.4\rL|1|N\r\nH|\\^&\nL|1\n");
        assert_eq!(
            events(&mut splitter),
            [
                RawEvent::Discarded {
                    bytes: 6,
                    reason: RawDiscardReason::OutsideMessage
                },
                message(b"H|\\^&\rR|1|^^^GLU|5.4\rL|1|N\r\n"),
                message(b"H|\\^&\nL|1\n"),
            ]
        );
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn reports_interrupted_and_truncated_messages() {
        let mut splitter = RawSplitter::default();
        splitter.push(b"H|\\^&\rP|1\rH|\\^&\rP|1\r");
        assert_eq!(
            events(&mut splitter),
            [RawEvent::Discarded {
                bytes: 10,
                reason: RawDiscardReason::Interrupted
            }]
        );
        assert!(splitter.in_message());
        assert_eq!(
            splitter.finish(),
            Some(RawEvent::Discarded {
                bytes: 10,
                reason: RawDiscardReason::Truncated
            })
        );
    }

    #[test]
    fn completes_a_final_terminator_without_line_ending() {
        let mut splitter = RawSplitter::default();
        splitter.push(b"H|\\^&\rL|1");
        assert!(events(&mut splitter).is_empty());
        assert_eq!(splitter.finish(), Some(message(b"H|\\^&\rL|1")));
    }

    #[test]
    fn discards_oversized_messages() {
        let mut splitter = RawSplitter::new(RawOptions {
            max_message_len: 12,
        });
        splitter.push(b"H|\\^&\rR|1|123456\rR|2\rL|1\rH|\\^&\rL|1\r");
        assert_eq!(
            events(&mut splitter),
            [
                RawEvent::Discarded {
                    bytes: 25,
                    reason: RawDiscardReason::TooLarge
                },
                message(b"H|\\^&\rL|1\r"),
            ]
        );
        let mut splitter = RawSplitter::new(RawOptions { max_message_len: 4 });
        splitter.push(b"0123456789");
        assert_eq!(
            events(&mut splitter),
            [RawEvent::Discarded {
                bytes: 10,
                reason: RawDiscardReason::TooLarge
            }]
        );
    }
}
