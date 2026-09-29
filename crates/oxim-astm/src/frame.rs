//! ASTM E1381 (CLSI LIS01-A2) frames.
//!
//! A frame is `<STX> FN text <ETB|ETX> C1 C2 <CR> <LF>`: a frame number from
//! `0` to `7`, at most 240 bytes of text, ETB for an intermediate frame or
//! ETX for the last frame of a message, and a checksum written as two
//! uppercase hexadecimal digits: the sum of every byte from the frame number
//! through ETB/ETX, modulo 256.

use crate::error::FrameError;

/// Start of text.
pub const STX: u8 = 0x02;
/// End of text: ends the last frame of a message.
pub const ETX: u8 = 0x03;
/// End of transmission: ends a session, or requests an interrupt when sent
/// instead of an acknowledgment.
pub const EOT: u8 = 0x04;
/// Enquiry: requests the link.
pub const ENQ: u8 = 0x05;
/// Positive acknowledgment.
pub const ACK: u8 = 0x06;
/// Negative acknowledgment.
pub const NAK: u8 = 0x15;
/// End of transmission block: ends an intermediate frame.
pub const ETB: u8 = 0x17;
/// Carriage return.
pub const CR: u8 = 0x0D;
/// Line feed.
pub const LF: u8 = 0x0A;

/// The largest frame text LIS01 allows.
pub const MAX_FRAME_TEXT: usize = 240;

/// Whether LIS01 forbids `byte` inside frame text: SOH, STX, ETX, EOT, ENQ,
/// ACK, DLE, NAK, SYN, ETB, LF and DC1–DC4. CR is allowed; it terminates
/// records.
pub fn is_restricted(byte: u8) -> bool {
    matches!(
        byte,
        0x01..=0x06 | 0x0A | 0x10..=0x17
    )
}

/// One LIS01 frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Frame {
    number: u8,
    text: Vec<u8>,
    last: bool,
}

impl Frame {
    /// Creates a frame. `number` must be from 0 to 7; `last` selects ETX
    /// (last frame of a message) instead of ETB.
    pub fn new(number: u8, text: Vec<u8>, last: bool) -> Result<Self, FrameError> {
        if number > 7 {
            return Err(FrameError::InvalidFrameNumber(number));
        }
        if let Some(position) = text.iter().position(|&b| is_restricted(b)) {
            return Err(FrameError::RestrictedCharacter {
                byte: text[position],
                position,
            });
        }
        Ok(Self { number, text, last })
    }

    /// The frame number, from 0 to 7.
    pub fn number(&self) -> u8 {
        self.number
    }

    /// The frame text.
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    /// Consumes the frame and returns its text.
    pub fn into_text(self) -> Vec<u8> {
        self.text
    }

    /// Whether this is the last frame of a message (ETX).
    pub fn is_last(&self) -> bool {
        self.last
    }

    /// The checksum of this frame.
    pub fn checksum(&self) -> u8 {
        checksum(b'0' + self.number, &self.text, self.terminator())
    }

    /// The frame on the wire.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.text.len() + 7);
        self.encode_into(&mut out);
        out
    }

    /// Appends the frame to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let sum = self.checksum();
        out.push(STX);
        out.push(b'0' + self.number);
        out.extend_from_slice(&self.text);
        out.push(self.terminator());
        out.push(HEX[usize::from(sum >> 4)]);
        out.push(HEX[usize::from(sum & 0x0F)]);
        out.push(CR);
        out.push(LF);
    }

    fn terminator(&self) -> u8 {
        if self.last { ETX } else { ETB }
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// The LIS01 checksum: the sum of the frame number character, the text and
/// the terminator (ETB or ETX), modulo 256.
pub fn checksum(number_char: u8, text: &[u8], terminator: u8) -> u8 {
    text.iter()
        .fold(number_char.wrapping_add(terminator), |sum, &b| {
            sum.wrapping_add(b)
        })
}

/// Splits a message into frames: one record (text up to and including its
/// CR) per frame, and records longer than `max_text` bytes across several
/// frames. Every frame that ends a record ends with ETX; the others end with
/// ETB. Numbering starts at `first_number` and wraps from 7 to 0.
pub fn split_message(
    message: &[u8],
    first_number: u8,
    max_text: usize,
) -> Result<Vec<Frame>, FrameError> {
    if first_number > 7 {
        return Err(FrameError::InvalidFrameNumber(first_number));
    }
    if let Some(position) = message.iter().position(|&b| is_restricted(b)) {
        return Err(FrameError::RestrictedCharacter {
            byte: message[position],
            position,
        });
    }
    let max_text = max_text.max(1);
    let mut frames = Vec::new();
    let mut number = first_number;
    for record in message.split_inclusive(|&b| b == CR) {
        let mut chunks = record.chunks(max_text).peekable();
        while let Some(chunk) = chunks.next() {
            frames.push(Frame {
                number,
                text: chunk.to_vec(),
                last: chunks.peek().is_none(),
            });
            number = (number + 1) % 8;
        }
    }
    Ok(frames)
}

/// The result of [`decode_frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// More bytes are needed.
    Incomplete,
    /// A valid frame occupying the first `len` bytes.
    Frame {
        /// The frame.
        frame: Frame,
        /// Bytes consumed.
        len: usize,
    },
    /// An invalid frame. Skip `len` bytes and continue.
    Invalid {
        /// What is wrong.
        error: FrameError,
        /// Bytes to skip. Link control characters that interrupted the frame
        /// (STX, ENQ, EOT) are not included, so they can be processed next.
        len: usize,
    },
}

/// Decodes one frame from the start of `input`.
pub fn decode_frame(input: &[u8], max_text: usize) -> Decoded {
    let Some(&first) = input.first() else {
        return Decoded::Incomplete;
    };
    if first != STX {
        return Decoded::Invalid {
            error: FrameError::MissingStx,
            len: 1,
        };
    }
    let Some(end) = input
        .iter()
        .skip(1)
        .position(|&b| b == ETX || b == ETB || is_restricted(b))
        .map(|i| i + 1)
    else {
        if input.len() > max_text + 2 {
            return Decoded::Invalid {
                error: FrameError::TooLong,
                len: input.len(),
            };
        }
        return Decoded::Incomplete;
    };
    let terminator = input[end];
    if terminator != ETX && terminator != ETB {
        return match terminator {
            STX | ENQ | EOT => Decoded::Invalid {
                error: FrameError::Interrupted(terminator),
                len: end,
            },
            byte => Decoded::Invalid {
                error: FrameError::RestrictedCharacter {
                    byte,
                    position: end,
                },
                len: end + 1,
            },
        };
    }
    let Some(&number_char) = input.get(1).filter(|_| end > 1) else {
        return Decoded::Invalid {
            error: FrameError::InvalidFrameNumber(terminator),
            len: end + 1,
        };
    };
    if !(b'0'..=b'7').contains(&number_char) {
        return Decoded::Invalid {
            error: FrameError::InvalidFrameNumber(number_char),
            len: end + 1,
        };
    }
    let text = &input[2..end];
    if text.len() > max_text {
        return Decoded::Invalid {
            error: FrameError::TooLong,
            len: end + 1,
        };
    }
    let Some(trailer) = input.get(end + 1..end + 5) else {
        return Decoded::Incomplete;
    };
    let received = match (hex_value(trailer[0]), hex_value(trailer[1])) {
        (Some(high), Some(low)) => high << 4 | low,
        _ => {
            return Decoded::Invalid {
                error: FrameError::MalformedChecksum,
                len: end + 1,
            };
        }
    };
    if trailer[2] != CR {
        return Decoded::Invalid {
            error: FrameError::MissingTrailer,
            len: end + 3,
        };
    }
    if trailer[3] != LF {
        return Decoded::Invalid {
            error: FrameError::MissingTrailer,
            len: end + 4,
        };
    }
    let expected = checksum(number_char, text, terminator);
    if expected != received {
        return Decoded::Invalid {
            error: FrameError::BadChecksum { expected, received },
            len: end + 5,
        };
    }
    Decoded::Frame {
        frame: Frame {
            number: number_char - b'0',
            text: text.to_vec(),
            last: terminator == ETX,
        },
        len: end + 5,
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_standard_checksum() {
        // "1H|\^&" + CR + ETX
        let frame = Frame::new(1, b"H|\\^&\r".to_vec(), true).unwrap();
        let sum = [b'1', b'H', b'|', b'\\', b'^', b'&', CR, ETX]
            .iter()
            .fold(0u8, |a, &b| a.wrapping_add(b));
        assert_eq!(frame.checksum(), sum);
        let encoded = frame.encode();
        assert_eq!(&encoded[..2], &[STX, b'1']);
        assert_eq!(&encoded[encoded.len() - 2..], &[CR, LF]);
        let hex = format!("{sum:02X}");
        assert_eq!(
            &encoded[encoded.len() - 4..encoded.len() - 2],
            hex.as_bytes()
        );
    }

    #[test]
    fn decodes_what_it_encodes() {
        let frame = Frame::new(7, b"R|1|^^^GLU|5.4\r".to_vec(), false).unwrap();
        let mut bytes = frame.encode();
        bytes.extend_from_slice(b"rest");
        assert_eq!(
            decode_frame(&bytes, MAX_FRAME_TEXT),
            Decoded::Frame {
                len: bytes.len() - 4,
                frame
            }
        );
    }

    #[test]
    fn accepts_lowercase_checksums() {
        // "1L|1<CR>" + ETX sums to 0x13A, so the checksum is "3A".
        let frame = Frame::new(1, b"L|1\r".to_vec(), true).unwrap();
        let mut bytes = frame.encode();
        let n = bytes.len();
        assert_eq!(&bytes[n - 4..n - 2], b"3A");
        bytes[n - 3] = b'a';
        assert!(matches!(
            decode_frame(&bytes, MAX_FRAME_TEXT),
            Decoded::Frame { .. }
        ));
    }

    #[test]
    fn waits_for_complete_frames() {
        let bytes = Frame::new(1, b"H|\\^&\r".to_vec(), true).unwrap().encode();
        for len in 0..bytes.len() {
            assert_eq!(
                decode_frame(&bytes[..len], MAX_FRAME_TEXT),
                Decoded::Incomplete
            );
        }
    }

    #[test]
    fn reports_invalid_frames() {
        let good = Frame::new(1, b"H|\\^&\r".to_vec(), true).unwrap().encode();
        let mut bad_sum = good.clone();
        let n = bad_sum.len();
        bad_sum[n - 3] = if bad_sum[n - 3] == b'0' { b'1' } else { b'0' };
        assert!(matches!(
            decode_frame(&bad_sum, MAX_FRAME_TEXT),
            Decoded::Invalid {
                error: FrameError::BadChecksum { .. },
                len
            } if len == n
        ));
        assert_eq!(
            decode_frame(b"\x029abc\x0300\r\n", MAX_FRAME_TEXT),
            Decoded::Invalid {
                error: FrameError::InvalidFrameNumber(b'9'),
                len: 6
            }
        );
        assert_eq!(
            decode_frame(b"\x021abc\x04", MAX_FRAME_TEXT),
            Decoded::Invalid {
                error: FrameError::Interrupted(EOT),
                len: 5
            }
        );
        assert_eq!(
            decode_frame(b"\x021a\nb\x03", MAX_FRAME_TEXT),
            Decoded::Invalid {
                error: FrameError::RestrictedCharacter {
                    byte: LF,
                    position: 3
                },
                len: 4
            }
        );
        assert_eq!(
            decode_frame(b"\x021abcdef", 3),
            Decoded::Invalid {
                error: FrameError::TooLong,
                len: 8
            }
        );
        assert_eq!(
            decode_frame(b"x", 3),
            Decoded::Invalid {
                error: FrameError::MissingStx,
                len: 1
            }
        );
    }

    #[test]
    fn splits_messages_into_frames() {
        let message = b"H|\\^&\rR|1|^^^GLU|5.4\rL|1\r";
        let frames = split_message(message, 1, MAX_FRAME_TEXT).unwrap();
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(Frame::is_last));
        assert_eq!(
            frames.iter().map(Frame::number).collect::<Vec<_>>(),
            [1, 2, 3]
        );

        let long = [b'x'; 10];
        let frames = split_message(&long, 6, 4).unwrap();
        assert_eq!(
            frames.iter().map(Frame::number).collect::<Vec<_>>(),
            [6, 7, 0]
        );
        assert_eq!(
            frames.iter().map(Frame::is_last).collect::<Vec<_>>(),
            [false, false, true]
        );
        let joined: Vec<u8> = frames.iter().flat_map(|f| f.text().to_vec()).collect();
        assert_eq!(joined, long);

        assert_eq!(
            split_message(b"a\nb", 1, 240),
            Err(FrameError::RestrictedCharacter {
                byte: LF,
                position: 1
            })
        );
    }
}
