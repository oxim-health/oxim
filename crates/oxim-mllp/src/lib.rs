//! Sans-IO MLLP framing for HL7 v2.
//!
//! MLLP (Minimal Lower Layer Protocol, HL7 v2 appendix C) wraps each message
//! in a start block (`0x0B`) and an end block followed by a carriage return
//! (`0x1C 0x0D`). This crate turns payloads into frames and a byte stream
//! into payloads. It performs no I/O: feed it the bytes you read from a
//! socket, serial line or file and act on the [`Event`]s it returns.
//!
//! MLLP release 2 transport acknowledgments (a frame holding a single `ACK`
//! or `NAK` byte) are recognized as [`Event::CommitAck`] and
//! [`Event::CommitNak`].
//!
//! ```
//! use oxim_mllp::{Decoder, Event, encode};
//!
//! let mut decoder = Decoder::default();
//! let frame = encode(b"MSH|^~\\&|LAB\r")?;
//! decoder.push(&frame[..5]);
//! assert_eq!(decoder.next_event(), None); // waiting for the rest
//! decoder.push(&frame[5..]);
//! assert_eq!(decoder.next_event(), Some(Event::Frame(b"MSH|^~\\&|LAB\r".to_vec())));
//! # Ok::<(), oxim_mllp::EncodeError>(())
//! ```

use memchr::{memchr, memchr2};
use thiserror::Error;

/// Start block character (vertical tab).
pub const START_BLOCK: u8 = 0x0B;
/// End block character (file separator).
pub const END_BLOCK: u8 = 0x1C;
/// Carriage return that follows the end block.
pub const CARRIAGE_RETURN: u8 = 0x0D;
/// MLLP release 2 positive commit acknowledgment frame.
pub const COMMIT_ACK: [u8; 4] = [START_BLOCK, 0x06, END_BLOCK, CARRIAGE_RETURN];
/// MLLP release 2 negative commit acknowledgment frame.
pub const COMMIT_NAK: [u8; 4] = [START_BLOCK, 0x15, END_BLOCK, CARRIAGE_RETURN];

/// Returned when a payload cannot be framed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EncodeError {
    /// The payload contains a start or end block byte, which would corrupt
    /// the framing.
    #[error("payload contains an MLLP block character at byte {position}")]
    BlockCharacter {
        /// Offset of the offending byte.
        position: usize,
    },
}

/// Frames `payload`.
pub fn encode(payload: &[u8]) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::with_capacity(payload.len() + 3);
    encode_into(payload, &mut out)?;
    Ok(out)
}

/// Appends the framed `payload` to `out`. Nothing is written on error.
pub fn encode_into(payload: &[u8], out: &mut Vec<u8>) -> Result<(), EncodeError> {
    if let Some(position) = memchr2(START_BLOCK, END_BLOCK, payload) {
        return Err(EncodeError::BlockCharacter { position });
    }
    out.reserve(payload.len() + 3);
    out.push(START_BLOCK);
    out.extend_from_slice(payload);
    out.push(END_BLOCK);
    out.push(CARRIAGE_RETURN);
    Ok(())
}

/// Why bytes were dropped by the [`Decoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscardReason {
    /// Bytes arrived outside a frame, for example keep-alive line breaks.
    OutsideFrame,
    /// A frame exceeded [`DecoderOptions::max_frame_len`].
    FrameTooLarge,
    /// A new start block arrived before the current frame ended.
    Interrupted,
    /// An end block was not followed by a carriage return while
    /// [`DecoderOptions::require_trailing_cr`] is set.
    MissingCarriageReturn,
    /// The stream ended inside a frame.
    Truncated,
}

/// Something the decoder found in the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A complete payload, without the framing bytes.
    Frame(Vec<u8>),
    /// An MLLP release 2 positive commit acknowledgment.
    CommitAck,
    /// An MLLP release 2 negative commit acknowledgment.
    CommitNak,
    /// Bytes that were dropped; the stream continues.
    Discarded {
        /// How many bytes were dropped.
        bytes: usize,
        /// Why they were dropped.
        reason: DiscardReason,
    },
}

/// Options for [`Decoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecoderOptions {
    /// The largest payload accepted. Larger frames are discarded without
    /// buffering them completely.
    pub max_frame_len: usize,
    /// Whether an end block must be followed by a carriage return. When
    /// `false`, an end block followed by any other byte also ends the frame,
    /// which tolerates senders that omit the carriage return.
    pub require_trailing_cr: bool,
}

impl Default for DecoderOptions {
    fn default() -> Self {
        Self {
            max_frame_len: 16 * 1024 * 1024,
            require_trailing_cr: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    InFrame,
    Skipping,
}

/// Incremental MLLP decoder.
///
/// Push bytes with [`Decoder::push`], then call [`Decoder::next_event`]
/// until it returns `None`. Call [`Decoder::finish`] when the stream ends.
#[derive(Debug, Clone)]
pub struct Decoder {
    options: DecoderOptions,
    buffer: Vec<u8>,
    /// Offset inside `buffer` from which to continue looking for the end of
    /// the current frame, so partial frames are not rescanned.
    scan: usize,
    state: State,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(DecoderOptions::default())
    }
}

impl Decoder {
    /// Creates a decoder.
    pub fn new(options: DecoderOptions) -> Self {
        Self {
            options,
            buffer: Vec::new(),
            scan: 0,
            state: State::Idle,
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

    /// Whether the decoder is inside a frame.
    pub fn in_frame(&self) -> bool {
        self.state == State::InFrame
    }

    /// Returns the next event, or `None` when more bytes are needed.
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            match self.state {
                State::Idle => match memchr(START_BLOCK, &self.buffer) {
                    Some(0) => {
                        self.buffer.drain(..1);
                        self.state = State::InFrame;
                        self.scan = 0;
                    }
                    Some(at) => return Some(self.discard(at, DiscardReason::OutsideFrame)),
                    None if self.buffer.is_empty() => return None,
                    None => {
                        return Some(self.discard(self.buffer.len(), DiscardReason::OutsideFrame));
                    }
                },
                State::Skipping => match memchr(START_BLOCK, &self.buffer) {
                    Some(0) => self.state = State::Idle,
                    Some(at) => {
                        self.state = State::Idle;
                        return Some(self.discard(at, DiscardReason::FrameTooLarge));
                    }
                    None if self.buffer.is_empty() => return None,
                    None => {
                        return Some(self.discard(self.buffer.len(), DiscardReason::FrameTooLarge));
                    }
                },
                State::InFrame => return self.frame_event(),
            }
        }
    }

    /// Signals the end of the stream. Returns a [`DiscardReason::Truncated`]
    /// event when a frame was left incomplete, and resets the decoder.
    ///
    /// Call [`Decoder::next_event`] until it returns `None` before calling
    /// this method.
    pub fn finish(&mut self) -> Option<Event> {
        let lenient_end = !self.options.require_trailing_cr
            && self.buffer.last() == Some(&END_BLOCK)
            && self.buffer.len() - 1 <= self.options.max_frame_len;
        let event = match self.state {
            // A lenient sender may close the connection right after the end
            // block without sending the carriage return.
            State::InFrame if lenient_end => {
                self.buffer.pop();
                Some(match self.buffer.as_slice() {
                    [0x06] => Event::CommitAck,
                    [0x15] => Event::CommitNak,
                    _ => Event::Frame(std::mem::take(&mut self.buffer)),
                })
            }
            State::InFrame => Some(Event::Discarded {
                // The start block was already consumed.
                bytes: self.buffer.len() + 1,
                reason: DiscardReason::Truncated,
            }),
            State::Skipping if !self.buffer.is_empty() => Some(Event::Discarded {
                bytes: self.buffer.len(),
                reason: DiscardReason::FrameTooLarge,
            }),
            _ if !self.buffer.is_empty() => Some(Event::Discarded {
                bytes: self.buffer.len(),
                reason: DiscardReason::OutsideFrame,
            }),
            _ => None,
        };
        self.buffer.clear();
        self.scan = 0;
        self.state = State::Idle;
        event
    }

    fn frame_event(&mut self) -> Option<Event> {
        let found =
            memchr2(END_BLOCK, START_BLOCK, &self.buffer[self.scan..]).map(|i| i + self.scan);
        let Some(at) = found else {
            if self.buffer.len() > self.options.max_frame_len {
                self.state = State::Skipping;
                // Count the start block that opened the frame.
                let event = self.discard(self.buffer.len(), DiscardReason::FrameTooLarge);
                return Some(add_bytes(event, 1));
            }
            self.scan = self.buffer.len();
            return None;
        };
        if self.buffer[at] == START_BLOCK {
            self.state = State::Idle;
            let event = self.discard(at, DiscardReason::Interrupted);
            return Some(add_bytes(event, 1));
        }
        let Some(&next) = self.buffer.get(at + 1) else {
            // The end block is the last byte: wait for the carriage return.
            self.scan = at;
            return None;
        };
        self.state = State::Idle;
        let trailer = match next {
            CARRIAGE_RETURN => 2,
            _ if self.options.require_trailing_cr => {
                let event = self.discard(at + 1, DiscardReason::MissingCarriageReturn);
                return Some(add_bytes(event, 1));
            }
            _ => 1,
        };
        let rest = self.buffer.split_off(at + trailer);
        let mut payload = std::mem::replace(&mut self.buffer, rest);
        payload.truncate(at);
        if payload.len() > self.options.max_frame_len {
            return Some(Event::Discarded {
                bytes: at + trailer + 1,
                reason: DiscardReason::FrameTooLarge,
            });
        }
        Some(match payload.as_slice() {
            [0x06] => Event::CommitAck,
            [0x15] => Event::CommitNak,
            _ => Event::Frame(payload),
        })
    }

    fn discard(&mut self, len: usize, reason: DiscardReason) -> Event {
        self.buffer.drain(..len);
        self.scan = 0;
        Event::Discarded { bytes: len, reason }
    }
}

fn add_bytes(event: Event, extra: usize) -> Event {
    match event {
        Event::Discarded { bytes, reason } => Event::Discarded {
            bytes: bytes + extra,
            reason,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(decoder: &mut Decoder) -> Vec<Event> {
        std::iter::from_fn(|| decoder.next_event()).collect()
    }

    fn frame(payload: &[u8]) -> Event {
        Event::Frame(payload.to_vec())
    }

    #[test]
    fn encodes_frames() {
        assert_eq!(encode(b"MSH").unwrap(), b"\x0bMSH\x1c\r");
        assert_eq!(
            encode(b"a\x1cb"),
            Err(EncodeError::BlockCharacter { position: 1 })
        );
        let mut out = b"x".to_vec();
        assert!(encode_into(b"\x0b", &mut out).is_err());
        assert_eq!(out, b"x");
    }

    #[test]
    fn decodes_several_frames_and_reports_junk() {
        let mut decoder = Decoder::default();
        decoder.push(b"\r\n\x0bA\x1c\r\x0bB\x1c\r\n\x0b\x06\x1c\r\x0b\x15\x1c\r");
        assert_eq!(
            events(&mut decoder),
            [
                Event::Discarded {
                    bytes: 2,
                    reason: DiscardReason::OutsideFrame
                },
                frame(b"A"),
                frame(b"B"),
                Event::Discarded {
                    bytes: 1,
                    reason: DiscardReason::OutsideFrame
                },
                Event::CommitAck,
                Event::CommitNak,
            ]
        );
        assert_eq!(decoder.buffered_len(), 0);
        assert_eq!(decoder.finish(), None);
    }

    #[test]
    fn waits_for_the_carriage_return() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x0bMSH\x1c");
        assert_eq!(decoder.next_event(), None);
        assert!(decoder.in_frame());
        decoder.push(b"\r");
        assert_eq!(decoder.next_event(), Some(frame(b"MSH")));
    }

    #[test]
    fn tolerates_or_rejects_a_missing_carriage_return() {
        let mut lenient = Decoder::default();
        lenient.push(b"\x0bA\x1c\x0bB\x1c\r");
        assert_eq!(events(&mut lenient), [frame(b"A"), frame(b"B")]);

        let options = DecoderOptions {
            require_trailing_cr: true,
            ..DecoderOptions::default()
        };
        let mut strict = Decoder::new(options);
        strict.push(b"\x0bA\x1c\x0bB\x1c\r");
        assert_eq!(
            events(&mut strict),
            [
                Event::Discarded {
                    bytes: 3,
                    reason: DiscardReason::MissingCarriageReturn
                },
                frame(b"B")
            ]
        );
    }

    #[test]
    fn reports_interrupted_frames() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x0bpartial\x0bwhole\x1c\r");
        assert_eq!(
            events(&mut decoder),
            [
                Event::Discarded {
                    bytes: 8,
                    reason: DiscardReason::Interrupted
                },
                frame(b"whole")
            ]
        );
    }

    #[test]
    fn discards_oversized_frames_without_buffering_them() {
        let options = DecoderOptions {
            max_frame_len: 4,
            ..DecoderOptions::default()
        };
        let mut decoder = Decoder::new(options);
        decoder.push(b"\x0b123456");
        assert_eq!(
            decoder.next_event(),
            Some(Event::Discarded {
                bytes: 7,
                reason: DiscardReason::FrameTooLarge
            })
        );
        assert_eq!(decoder.buffered_len(), 0);
        decoder.push(b"789\x1c\r\x0bok\x1c\r");
        assert_eq!(
            events(&mut decoder),
            [
                Event::Discarded {
                    bytes: 5,
                    reason: DiscardReason::FrameTooLarge
                },
                frame(b"ok")
            ]
        );
    }

    #[test]
    fn rejects_oversized_frames_that_arrive_at_once() {
        let options = DecoderOptions {
            max_frame_len: 2,
            ..DecoderOptions::default()
        };
        let mut decoder = Decoder::new(options);
        decoder.push(b"\x0babc\x1c\r\x0bok\x1c\r");
        assert_eq!(
            events(&mut decoder),
            [
                Event::Discarded {
                    bytes: 6,
                    reason: DiscardReason::FrameTooLarge
                },
                frame(b"ok")
            ]
        );
    }

    #[test]
    fn accepts_a_final_frame_without_carriage_return() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x0bA\x1c");
        assert_eq!(decoder.next_event(), None);
        assert_eq!(decoder.finish(), Some(frame(b"A")));

        let options = DecoderOptions {
            require_trailing_cr: true,
            ..DecoderOptions::default()
        };
        let mut strict = Decoder::new(options);
        strict.push(b"\x0bA\x1c");
        assert_eq!(strict.next_event(), None);
        assert_eq!(
            strict.finish(),
            Some(Event::Discarded {
                bytes: 3,
                reason: DiscardReason::Truncated
            })
        );
    }

    #[test]
    fn reports_truncated_streams() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x0bMSH|");
        assert_eq!(decoder.next_event(), None);
        assert_eq!(
            decoder.finish(),
            Some(Event::Discarded {
                bytes: 5,
                reason: DiscardReason::Truncated
            })
        );
        assert!(!decoder.in_frame());
    }
}
