//! The fixed-width transaction headers and the segment identifiers.

/// A field of a fixed-width transaction header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeaderField {
    /// The two-character field identifier from the data dictionary.
    pub id: &'static str,
    /// The data dictionary name.
    pub name: &'static str,
    /// The byte offset in the header.
    pub start: usize,
    /// The width in bytes.
    pub len: usize,
}

const fn field(id: &'static str, name: &'static str, start: usize, len: usize) -> HeaderField {
    HeaderField {
        id,
        name,
        start,
        len,
    }
}

/// The 56-byte request header (Telecommunication Standard D.0; 5.1 uses
/// the same layout).
pub const REQUEST_HEADER: [HeaderField; 9] = [
    field("A1", "BIN Number", 0, 6),
    field("A2", "Version/Release Number", 6, 2),
    field("A3", "Transaction Code", 8, 2),
    field("A4", "Processor Control Number", 10, 10),
    field("A9", "Transaction Count", 20, 1),
    field("B2", "Service Provider ID Qualifier", 21, 2),
    field("B1", "Service Provider ID", 23, 15),
    field("D1", "Date of Service", 38, 8),
    field("AK", "Software Vendor/Certification ID", 46, 10),
];

/// The 31-byte response header.
pub const RESPONSE_HEADER: [HeaderField; 7] = [
    field("A2", "Version/Release Number", 0, 2),
    field("A3", "Transaction Code", 2, 2),
    field("A9", "Transaction Count", 4, 1),
    field("F1", "Header Response Status", 5, 1),
    field("B2", "Service Provider ID Qualifier", 6, 2),
    field("B1", "Service Provider ID", 8, 15),
    field("D1", "Date of Service", 23, 8),
];

/// Length of the request header.
pub const REQUEST_HEADER_LEN: usize = 56;
/// Length of the response header.
pub const RESPONSE_HEADER_LEN: usize = 31;

/// Whether a transmission is a request or a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeaderKind {
    /// A request from the pharmacy (56-byte header).
    Request,
    /// A response from the processor (31-byte header).
    Response,
}

impl HeaderKind {
    /// The header layout.
    pub fn fields(self) -> &'static [HeaderField] {
        match self {
            Self::Request => &REQUEST_HEADER,
            Self::Response => &RESPONSE_HEADER,
        }
    }

    /// The header length.
    pub fn header_len(self) -> usize {
        match self {
            Self::Request => REQUEST_HEADER_LEN,
            Self::Response => RESPONSE_HEADER_LEN,
        }
    }

    /// The layout field with identifier `id`.
    pub fn field(self, id: &str) -> Option<&'static HeaderField> {
        self.fields().iter().find(|field| field.id == id)
    }
}

/// The name of a segment identifier (field 111-AM), for example `AM07`
/// (Claim). The names follow the D.0 data dictionary and are informative.
pub fn segment_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "AM01" => "Patient",
        "AM02" => "Pharmacy Provider",
        "AM03" => "Prescriber",
        "AM04" => "Insurance",
        "AM05" => "Coordination of Benefits/Other Payments",
        "AM06" => "Workers' Compensation",
        "AM07" => "Claim",
        "AM08" => "DUR/PPS",
        "AM09" => "Coupon",
        "AM10" => "Compound",
        "AM11" => "Pricing",
        "AM12" => "Prior Authorization",
        "AM13" => "Clinical",
        "AM14" => "Additional Documentation",
        "AM15" => "Facility",
        "AM16" => "Narrative",
        "AM20" => "Response Message",
        "AM21" => "Response Status",
        "AM22" => "Response Claim",
        "AM23" => "Response Pricing",
        "AM24" => "Response DUR/PPS",
        "AM25" => "Response Insurance",
        "AM26" => "Response Prior Authorization",
        "AM27" => "Response Insurance Additional Information",
        "AM28" => "Response Coordination of Benefits/Other Payers",
        "AM29" => "Response Patient",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_cover_the_header_exactly() {
        for kind in [HeaderKind::Request, HeaderKind::Response] {
            let mut end = 0;
            for field in kind.fields() {
                assert_eq!(field.start, end, "{}", field.id);
                end += field.len;
            }
            assert_eq!(end, kind.header_len());
        }
        assert_eq!(HeaderKind::Request.field("B1").unwrap().len, 15);
        assert!(HeaderKind::Response.field("A1").is_none());
        assert_eq!(segment_name("AM07"), Some("Claim"));
        assert_eq!(segment_name("AM99"), None);
    }
}
