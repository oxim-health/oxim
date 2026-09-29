//! Character sets declared in MSH-18 (HL7 table 0211).

use encoding_rs::Encoding;

use crate::error::CharsetError;

/// Resolves an HL7 character set name, as found in MSH-18, to a text encoding.
///
/// HL7 table 0211 names are recognized first (`ASCII`, `8859/1` ... `8859/15`,
/// `UNICODE UTF-8`, ...). Other names are looked up as WHATWG encoding labels,
/// because many senders write `UTF-8` or `windows-1254` instead of the table
/// value.
///
/// Only encodings in which delimiter bytes can never occur inside a
/// multi-byte character are accepted, because HL7 v2 is split at byte level.
/// Big5, GB 18030, Shift_JIS, ISO-2022 and UTF-16/32 are rejected with
/// [`CharsetError::Unsupported`].
///
/// A declared `ASCII` resolves to UTF-8: it decodes pure ASCII identically and
/// is the most common reality when such senders emit non-ASCII bytes anyway.
pub fn encoding_for_charset(name: &[u8]) -> Result<&'static Encoding, CharsetError> {
    let trimmed = name.trim_ascii();
    let upper = trimmed.to_ascii_uppercase();
    let label: Option<&[u8]> = match upper.as_slice() {
        b"ASCII" | b"ISO IR6" | b"UNICODE" | b"UNICODE UTF-8" | b"ISO IR192" => Some(b"utf-8"),
        b"8859/1" | b"ISO IR100" => Some(b"iso-8859-1"),
        b"8859/2" => Some(b"iso-8859-2"),
        b"8859/3" => Some(b"iso-8859-3"),
        b"8859/4" => Some(b"iso-8859-4"),
        b"8859/5" => Some(b"iso-8859-5"),
        b"8859/6" => Some(b"iso-8859-6"),
        b"8859/7" => Some(b"iso-8859-7"),
        b"8859/8" => Some(b"iso-8859-8"),
        b"8859/9" => Some(b"iso-8859-9"),
        b"8859/15" => Some(b"iso-8859-15"),
        b"KS X 1001" => Some(b"euc-kr"),
        b"UNICODE UTF-16" | b"UNICODE UTF-32" | b"GB 18030-2000" | b"BIG-5" | b"CNS 11643-1992"
        | b"ISO IR14" | b"ISO IR87" | b"ISO IR159" | b"ISO IR58" => {
            return Err(CharsetError::Unsupported(lossy(trimmed)));
        }
        _ => None,
    };
    let encoding = Encoding::for_label(label.unwrap_or(trimmed))
        .ok_or_else(|| CharsetError::Unknown(lossy(trimmed)))?;
    if is_byte_safe(encoding) {
        Ok(encoding)
    } else {
        Err(CharsetError::Unsupported(lossy(trimmed)))
    }
}

/// Whether HL7 delimiters (printable ASCII) can never appear as part of a
/// multi-byte character in `encoding`.
fn is_byte_safe(encoding: &'static Encoding) -> bool {
    use encoding_rs::{
        BIG5, GB18030, GBK, ISO_2022_JP, REPLACEMENT, SHIFT_JIS, UTF_16BE, UTF_16LE,
    };
    ![
        BIG5,
        GB18030,
        GBK,
        ISO_2022_JP,
        REPLACEMENT,
        SHIFT_JIS,
        UTF_16BE,
        UTF_16LE,
    ]
    .contains(&encoding)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use encoding_rs::{EUC_KR, ISO_8859_2, UTF_8, WINDOWS_1252, WINDOWS_1254};

    use super::*;

    #[test]
    fn resolves_table_0211_names() {
        assert_eq!(encoding_for_charset(b"ASCII"), Ok(UTF_8));
        assert_eq!(encoding_for_charset(b"UNICODE UTF-8"), Ok(UTF_8));
        assert_eq!(encoding_for_charset(b"8859/1"), Ok(WINDOWS_1252));
        assert_eq!(encoding_for_charset(b"8859/2"), Ok(ISO_8859_2));
        assert_eq!(encoding_for_charset(b"8859/9"), Ok(WINDOWS_1254));
        assert_eq!(encoding_for_charset(b" 8859/9 "), Ok(WINDOWS_1254));
        assert_eq!(encoding_for_charset(b"KS X 1001"), Ok(EUC_KR));
    }

    #[test]
    fn resolves_common_labels() {
        assert_eq!(encoding_for_charset(b"UTF-8"), Ok(UTF_8));
        assert_eq!(encoding_for_charset(b"utf8"), Ok(UTF_8));
        assert_eq!(encoding_for_charset(b"windows-1254"), Ok(WINDOWS_1254));
        assert_eq!(encoding_for_charset(b"ISO-8859-9"), Ok(WINDOWS_1254));
    }

    #[test]
    fn rejects_unsafe_encodings() {
        for name in [
            &b"BIG-5"[..],
            b"GB 18030-2000",
            b"UNICODE UTF-16",
            b"shift_jis",
            b"gbk",
            b"utf-16le",
        ] {
            assert!(
                matches!(
                    encoding_for_charset(name),
                    Err(CharsetError::Unsupported(_))
                ),
                "{}",
                String::from_utf8_lossy(name)
            );
        }
    }

    #[test]
    fn reports_unknown_names() {
        assert_eq!(
            encoding_for_charset(b"KLINGON"),
            Err(CharsetError::Unknown("KLINGON".into()))
        );
    }
}
