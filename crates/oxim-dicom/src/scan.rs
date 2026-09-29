//! A structural check of encoded data sets.
//!
//! The dicom-rs parser allocates every value at its declared length and
//! recurses into nested sequences, so a hostile object could make it
//! allocate gigabytes or overflow the stack. Data sets are therefore walked
//! here first: every declared length must fit in the enclosing bytes and
//! sequences may nest at most [`MAX_DEPTH`] levels. The same walk extracts
//! top-level values cheaply, without building an object.

use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::{Tag, VR};
use dicom_dictionary_std::StandardDataDictionary;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex, TransferSyntaxRegistry};

use crate::error::DicomError;
use crate::part10::{is_long_vr, trimmed_text};
use crate::uids;

/// How deeply sequences may nest.
pub(crate) const MAX_DEPTH: usize = 32;

/// A tag as (group, element).
pub(crate) type TagNumber = (u16, u16);

const UNDEFINED: u32 = u32::MAX;
const ITEM: TagNumber = (0xFFFE, 0xE000);
const ITEM_END: TagNumber = (0xFFFE, 0xE00D);
const SEQUENCE_END: TagNumber = (0xFFFE, 0xE0DD);
const PIXEL_DATA: TagNumber = (0x7FE0, 0x0010);

/// The encoding rules of a data set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// Implicit VR Little Endian.
    ImplicitLittle,
    /// Explicit VR Little Endian, also used by every compressed syntax.
    ExplicitLittle,
    /// Explicit VR Big Endian.
    ExplicitBig,
}

impl Syntax {
    /// The encoding of a transfer syntax that this crate can read.
    pub(crate) fn of(transfer_syntax: &str) -> Result<Self, DicomError> {
        match transfer_syntax.trim_end_matches(['\0', ' ']) {
            uids::IMPLICIT_VR_LITTLE_ENDIAN => Ok(Self::ImplicitLittle),
            uids::EXPLICIT_VR_BIG_ENDIAN => Ok(Self::ExplicitBig),
            other => match TransferSyntaxRegistry.get(other) {
                Some(ts) if !ts.is_unsupported() => Ok(Self::ExplicitLittle),
                Some(ts) => Err(DicomError::Unsupported(format!(
                    "transfer syntax {} ({other}) is not supported",
                    ts.name()
                ))),
                None => Err(DicomError::Unsupported(format!(
                    "unknown transfer syntax {other}"
                ))),
            },
        }
    }

    fn big_endian(self) -> bool {
        self == Self::ExplicitBig
    }
}

struct Header {
    tag: (u16, u16),
    vr: Option<[u8; 2]>,
    length: u32,
}

struct Walk<'a, F> {
    data: &'a [u8],
    pos: usize,
    visit: F,
}

fn invalid(message: &str, pos: usize) -> DicomError {
    DicomError::Invalid(format!("{message} at byte {pos} of the data set"))
}

impl<'a, F: FnMut((u16, u16), usize, &'a [u8])> Walk<'a, F> {
    fn take(&mut self, n: usize, end: usize) -> Result<&'a [u8], DicomError> {
        let stop = self
            .pos
            .checked_add(n)
            .filter(|stop| *stop <= end)
            .ok_or_else(|| invalid("truncated element", self.pos))?;
        let bytes = self
            .data
            .get(self.pos..stop)
            .ok_or_else(|| invalid("truncated element", self.pos))?;
        self.pos = stop;
        Ok(bytes)
    }

    fn u16(&mut self, big: bool, end: usize) -> Result<u16, DicomError> {
        let bytes = self.take(2, end)?;
        let pair = [bytes[0], bytes[1]];
        Ok(if big {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    }

    fn u32(&mut self, big: bool, end: usize) -> Result<u32, DicomError> {
        let bytes = self.take(4, end)?;
        let quad = [bytes[0], bytes[1], bytes[2], bytes[3]];
        Ok(if big {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        })
    }

    fn header(&mut self, syntax: Syntax, end: usize) -> Result<Header, DicomError> {
        let big = syntax.big_endian();
        let tag = (self.u16(big, end)?, self.u16(big, end)?);
        if tag.0 == 0xFFFE || syntax == Syntax::ImplicitLittle {
            let length = self.u32(big, end)?;
            return Ok(Header {
                tag,
                vr: None,
                length,
            });
        }
        let at = self.pos;
        let vr = self.take(2, end)?;
        let vr = [vr[0], vr[1]];
        if !vr.iter().all(u8::is_ascii_uppercase) {
            return Err(invalid("invalid value representation", at));
        }
        let length = if is_long_vr(vr) {
            self.take(2, end)?;
            self.u32(big, end)?
        } else {
            u32::from(self.u16(big, end)?)
        };
        Ok(Header {
            tag,
            vr: Some(vr),
            length,
        })
    }

    /// The end of a value of defined `length` starting here.
    fn value_end(&self, length: u32, end: usize) -> Result<usize, DicomError> {
        usize::try_from(length)
            .ok()
            .and_then(|length| self.pos.checked_add(length))
            .filter(|stop| *stop <= end)
            .ok_or_else(|| invalid("a declared length exceeds the available data", self.pos))
    }

    /// Reads data elements up to `end`, or up to an item delimitation item
    /// when `delimited`.
    fn elements(
        &mut self,
        end: usize,
        syntax: Syntax,
        depth: usize,
        delimited: bool,
    ) -> Result<(), DicomError> {
        while self.pos < end {
            let at = self.pos;
            let header = self.header(syntax, end)?;
            if header.tag == ITEM_END && delimited {
                return Ok(());
            }
            if header.tag.0 == 0xFFFE {
                return Err(invalid("unexpected item or delimiter", at));
            }
            self.element(&header, end, syntax, depth)?;
        }
        if delimited {
            return Err(invalid("an item has no delimitation item", self.pos));
        }
        Ok(())
    }

    fn element(
        &mut self,
        header: &Header,
        end: usize,
        syntax: Syntax,
        depth: usize,
    ) -> Result<(), DicomError> {
        let dictionary_sequence = || {
            StandardDataDictionary
                .by_tag(Tag(header.tag.0, header.tag.1))
                .and_then(|entry| entry.vr().exact())
                == Some(VR::SQ)
        };
        if header.length == UNDEFINED {
            return match header.vr {
                Some(vr) if header.tag == PIXEL_DATA && vr != *b"SQ" => self.fragments(end, syntax),
                // Undefined-length UN holds an Implicit VR Little Endian
                // sequence (PS3.5 6.2.2).
                Some(vr) if vr == *b"UN" => {
                    self.sequence(end, true, Syntax::ImplicitLittle, depth + 1)
                }
                Some(vr) if vr == *b"SQ" => self.sequence(end, true, syntax, depth + 1),
                Some(_) => Err(invalid(
                    "undefined length on a non-sequence element",
                    self.pos,
                )),
                None => self.sequence(end, true, syntax, depth + 1),
            };
        }
        let stop = self.value_end(header.length, end)?;
        let is_sequence = match header.vr {
            Some(vr) => vr == *b"SQ",
            None => dictionary_sequence(),
        };
        if is_sequence {
            self.sequence(stop, false, syntax, depth + 1)?;
        } else {
            let value = &self.data[self.pos..stop];
            (self.visit)(header.tag, depth, value);
            self.pos = stop;
        }
        Ok(())
    }

    /// Reads sequence items up to `end`, or up to a sequence delimitation
    /// item when `delimited`.
    fn sequence(
        &mut self,
        end: usize,
        delimited: bool,
        syntax: Syntax,
        depth: usize,
    ) -> Result<(), DicomError> {
        if depth > MAX_DEPTH {
            return Err(invalid("sequences are nested too deeply", self.pos));
        }
        loop {
            if self.pos >= end {
                if delimited {
                    return Err(invalid("a sequence has no delimitation item", self.pos));
                }
                return Ok(());
            }
            let at = self.pos;
            let header = self.header_of_item(syntax, end)?;
            match header.tag {
                ITEM if header.length == UNDEFINED => self.elements(end, syntax, depth, true)?,
                ITEM => {
                    let stop = self.value_end(header.length, end)?;
                    self.elements(stop, syntax, depth, false)?;
                }
                SEQUENCE_END if delimited => return Ok(()),
                _ => return Err(invalid("expected a sequence item", at)),
            }
        }
    }

    /// Reads encapsulated pixel data fragments up to the sequence
    /// delimitation item.
    fn fragments(&mut self, end: usize, syntax: Syntax) -> Result<(), DicomError> {
        loop {
            let at = self.pos;
            if at >= end {
                return Err(invalid("pixel data has no delimitation item", at));
            }
            let header = self.header_of_item(syntax, end)?;
            match header.tag {
                ITEM if header.length != UNDEFINED => {
                    self.pos = self.value_end(header.length, end)?;
                }
                SEQUENCE_END => return Ok(()),
                _ => return Err(invalid("expected a pixel data fragment", at)),
            }
        }
    }

    /// An item or delimiter header: tag and 32-bit length, without VR.
    fn header_of_item(&mut self, syntax: Syntax, end: usize) -> Result<Header, DicomError> {
        let big = syntax.big_endian();
        let tag = (self.u16(big, end)?, self.u16(big, end)?);
        let length = self.u32(big, end)?;
        Ok(Header {
            tag,
            vr: None,
            length,
        })
    }
}

/// Walks `data`, calling `visit(tag, depth, value)` for every element that
/// is not a sequence or encapsulated pixel data.
pub(crate) fn walk<'a>(
    data: &'a [u8],
    syntax: Syntax,
    visit: impl FnMut((u16, u16), usize, &'a [u8]),
) -> Result<(), DicomError> {
    let mut walk = Walk {
        data,
        pos: 0,
        visit,
    };
    walk.elements(data.len(), syntax, 0, false)
}

/// Checks that `data` is a structurally sound data set.
pub(crate) fn check(data: &[u8], syntax: Syntax) -> Result<(), DicomError> {
    walk(data, syntax, |_, _, _| {})
}

/// The text of top-level elements with the given tags.
pub(crate) fn top_level_text(
    data: &[u8],
    syntax: Syntax,
    tags: &[TagNumber],
) -> Result<Vec<(TagNumber, String)>, DicomError> {
    let mut found = Vec::new();
    walk(data, syntax, |tag, depth, value| {
        if depth == 0 && tags.contains(&tag) {
            found.push((tag, trimmed_text(value)));
        }
    })?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explicit(group: u16, element: u16, vr: &[u8; 2], value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&group.to_le_bytes());
        out.extend_from_slice(&element.to_le_bytes());
        out.extend_from_slice(vr);
        if is_long_vr(*vr) {
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        } else {
            out.extend_from_slice(&(value.len() as u16).to_le_bytes());
        }
        out.extend_from_slice(value);
        out
    }

    fn item(content: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFE, 0xFF, 0x00, 0xE0];
        out.extend_from_slice(&(content.len() as u32).to_le_bytes());
        out.extend_from_slice(content);
        out
    }

    #[test]
    fn walks_nested_sequences() {
        let inner = explicit(0x0008, 0x1155, b"UI", b"1.2.3\0");
        let mut data = explicit(0x0008, 0x0060, b"CS", b"CT");
        data.extend(explicit(0x0008, 0x1140, b"SQ", &item(&inner)));
        // An undefined-length sequence with an undefined-length item.
        data.extend_from_slice(&[0x40, 0x00, 0x75, 0x02, b'S', b'Q', 0, 0]);
        data.extend_from_slice(&UNDEFINED.to_le_bytes());
        data.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
        data.extend_from_slice(&UNDEFINED.to_le_bytes());
        data.extend(explicit(0x0040, 0x1001, b"SH", b"RP1 "));
        data.extend_from_slice(&[0xFE, 0xFF, 0x0D, 0xE0, 0, 0, 0, 0]);
        data.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
        data.extend(explicit(0x0020, 0x000D, b"UI", b"1.2.9\0"));
        let found = top_level_text(
            &data,
            Syntax::ExplicitLittle,
            &[(0x0008, 0x0060), (0x0020, 0x000D), (0x0008, 0x1155)],
        )
        .unwrap();
        assert_eq!(
            found,
            [
                ((0x0008, 0x0060), "CT".to_owned()),
                ((0x0020, 0x000D), "1.2.9".to_owned()),
            ]
        );
    }

    #[test]
    fn walks_encapsulated_pixel_data() {
        let mut data = explicit(0x0028, 0x0010, b"US", &[1, 0]);
        data.extend_from_slice(&[0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0]);
        data.extend_from_slice(&UNDEFINED.to_le_bytes());
        data.extend(item(b""));
        data.extend(item(b"\xFF\xD8\xFF\xD9"));
        data.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
        check(&data, Syntax::ExplicitLittle).unwrap();
    }

    #[test]
    fn walks_implicit_vr() {
        let mut data = Vec::new();
        // (0008,1140) Referenced Image Sequence, a sequence by dictionary.
        let inner = [&[0x08, 0x00, 0x55, 0x11, 6, 0, 0, 0][..], b"1.2.3\0"].concat();
        let sequence = item(&inner);
        data.extend_from_slice(&[0x08, 0x00, 0x40, 0x11]);
        data.extend_from_slice(&(sequence.len() as u32).to_le_bytes());
        data.extend(sequence);
        check(&data, Syntax::ImplicitLittle).unwrap();
        // The same bytes with a broken item length.
        let mut broken = data.clone();
        broken[12] = 0xF0;
        assert!(check(&broken, Syntax::ImplicitLittle).is_err());
    }

    #[test]
    fn rejects_oversized_lengths_and_deep_nesting() {
        let mut data = explicit(0x0008, 0x0060, b"CS", b"CT");
        data.truncate(data.len() - 1);
        assert!(check(&data, Syntax::ExplicitLittle).is_err());
        let mut huge = vec![0xE0, 0x7F, 0x10, 0x00, b'O', b'W', 0, 0];
        huge.extend_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(check(&huge, Syntax::ExplicitLittle).is_err());
        let mut nested = Vec::new();
        for _ in 0..=MAX_DEPTH + 1 {
            nested.extend_from_slice(&[0x08, 0x00, 0x40, 0x11, b'S', b'Q', 0, 0]);
            nested.extend_from_slice(&UNDEFINED.to_le_bytes());
            nested.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
            nested.extend_from_slice(&UNDEFINED.to_le_bytes());
        }
        let error = check(&nested, Syntax::ExplicitLittle).unwrap_err();
        assert!(error.to_string().contains("nested too deeply"), "{error}");
    }

    #[test]
    fn knows_transfer_syntaxes() {
        assert_eq!(
            Syntax::of(uids::IMPLICIT_VR_LITTLE_ENDIAN).unwrap(),
            Syntax::ImplicitLittle
        );
        assert_eq!(
            Syntax::of("1.2.840.10008.1.2.4.50").unwrap(),
            Syntax::ExplicitLittle
        );
        assert!(matches!(
            Syntax::of("1.2.840.10008.1.2.1.99"),
            Err(DicomError::Unsupported(_))
        ));
        assert!(Syntax::of("1.2.3.4").is_err());
    }
}
