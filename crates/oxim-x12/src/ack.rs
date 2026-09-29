//! Acknowledgments: `TA1` (interchange), `997` (functional) and `999`
//! (implementation).
//!
//! The builders read the envelope of a received interchange and write a
//! complete acknowledgment interchange with sender and receiver swapped.
//! Envelope issues found by [`Interchange::validate`] are reported
//! automatically; errors found by the caller (for example against an
//! implementation guide) are added through [`TransactionAck`]. The caller
//! supplies control numbers, the date and the time: nothing here reads the
//! clock.

use thiserror::Error;

use crate::envelope::{GroupEnvelope, InterchangeEnvelope, Issue, IssueKind};
use crate::error::PathError;
use crate::interchange::Interchange;

/// Returned when an acknowledgment cannot be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AckError {
    /// The received input has no interchange at the given position.
    #[error("the input has no interchange {0}")]
    NoInterchange(usize),
    /// The interchange has no functional group at the given position.
    #[error("the interchange has no functional group {0}")]
    NoGroup(usize),
    /// The date or time is not in the required form.
    #[error("{0}")]
    InvalidDateTime(&'static str),
    /// A value cannot be written, for example because it contains a
    /// delimiter.
    #[error(transparent)]
    Value(#[from] PathError),
}

/// Header values of an acknowledgment interchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckHeader<'a> {
    /// ISA-13 of the acknowledgment (at most 9 digits).
    pub control_number: u32,
    /// The date as `CCYYMMDD`.
    pub date: &'a str,
    /// The time as `HHMM`.
    pub time: &'a str,
}

impl AckHeader<'_> {
    fn check(&self) -> Result<(), AckError> {
        let digits =
            |text: &str, len| text.len() == len && text.bytes().all(|b| b.is_ascii_digit());
        if !digits(self.date, 8) {
            return Err(AckError::InvalidDateTime("the date must be CCYYMMDD"));
        }
        if !digits(self.time, 4) {
            return Err(AckError::InvalidDateTime("the time must be HHMM"));
        }
        if self.control_number > 999_999_999 {
            return Err(AckError::InvalidDateTime(
                "the control number must have at most 9 digits",
            ));
        }
        Ok(())
    }
}

/// An acknowledgment code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AckCode {
    /// `A`: accepted.
    Accepted,
    /// `E`: accepted, but errors were noted.
    AcceptedWithErrors,
    /// `R`: rejected.
    Rejected,
}

impl AckCode {
    /// The code letter.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "A",
            Self::AcceptedWithErrors => "E",
            Self::Rejected => "R",
        }
    }

    fn is_accepted(self) -> bool {
        !matches!(self, Self::Rejected)
    }
}

/// Options for [`build_ta1`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ta1Options<'a> {
    /// The acknowledgment's own header values.
    pub header: AckHeader<'a>,
    /// Which interchange of the input to acknowledge (0-based).
    pub interchange: usize,
    /// TA1-04; derived from the envelope issues when `None`.
    pub code: Option<AckCode>,
    /// TA1-05, the interchange note code (`000` for no error); derived
    /// from the envelope issues when `None`.
    pub note: Option<&'a str>,
}

/// Options for [`build_functional_ack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionalAckOptions<'a> {
    /// `997` or `999`.
    pub kind: FunctionalAckKind,
    /// The acknowledgment's own interchange header values.
    pub header: AckHeader<'a>,
    /// GS-06 of the acknowledgment's functional group.
    pub group_control_number: u32,
    /// Which interchange of the input to acknowledge (0-based).
    pub interchange: usize,
    /// Which functional group of that interchange (0-based).
    pub group: usize,
    /// Results for transaction sets, replacing the derived result of the
    /// same transaction set. Transaction sets without an entry are accepted
    /// unless their envelope has issues.
    pub transactions: Vec<TransactionAck>,
}

/// The kind of functional acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionalAckKind {
    /// `997` functional acknowledgment (version 4010 and earlier).
    Ack997,
    /// `999` implementation acknowledgment (version 5010, `005010X231A1`).
    Ack999,
}

/// The result for one transaction set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionAck {
    /// The transaction set's position in the group (0-based).
    pub transaction: usize,
    /// AK5-01 / IK5-01.
    pub code: AckCode,
    /// AK5-02 … AK5-06 / IK5-02 … IK5-06 syntax error codes, for example
    /// `5` (one or more segments in error).
    pub syntax_errors: Vec<String>,
    /// Segment errors (AK3 / IK3 with their AK4 / IK4).
    pub segments: Vec<SegmentError>,
}

/// An error in one segment of a transaction set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentError {
    /// AK3-01 / IK3-01: the segment identifier.
    pub segment_id: String,
    /// AK3-02 / IK3-02: the position in the transaction set, `ST` = 1.
    pub position: usize,
    /// AK3-03 / IK3-03: the loop identifier, if known.
    pub loop_id: Option<String>,
    /// AK3-04 / IK3-04: the segment syntax error code, for example `8`
    /// (segment has data element errors).
    pub code: String,
    /// Element errors (AK4 / IK4).
    pub elements: Vec<ElementError>,
}

/// An error in one element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementError {
    /// The element position.
    pub position: usize,
    /// The component position, if the error is in a component.
    pub component: Option<usize>,
    /// The repetition, if the element repeats (999 only).
    pub repetition: Option<usize>,
    /// The data element reference number, if known.
    pub reference: Option<String>,
    /// The element syntax error code, for example `7` (invalid code value).
    pub code: String,
    /// A copy of the bad value.
    pub bad_value: Option<String>,
}

fn pad(text: &str, len: usize) -> String {
    format!("{text:<len$}")
}

fn envelope_of(input: &Interchange, index: usize) -> Result<InterchangeEnvelope, AckError> {
    input
        .envelope()
        .interchanges
        .into_iter()
        .nth(index)
        .ok_or(AckError::NoInterchange(index))
}

/// The ISA of an acknowledgment for `received`: sender and receiver
/// swapped, same version and usage indicator.
fn isa(input: &Interchange, received: &InterchangeEnvelope, header: &AckHeader<'_>) -> Vec<String> {
    let isa11 = if input.delimiters().repetition.is_some() {
        String::new()
    } else {
        "U".to_owned()
    };
    vec![
        "ISA".into(),
        "00".into(),
        pad("", 10),
        "00".into(),
        pad("", 10),
        pad(&received.receiver.0, 2),
        pad(&received.receiver.1, 15),
        pad(&received.sender.0, 2),
        pad(&received.sender.1, 15),
        header.date[2..].to_owned(),
        header.time.to_owned(),
        isa11,
        pad(&received.version, 5),
        format!("{:09}", header.control_number),
        "0".into(),
        if received.usage.is_empty() {
            "P".into()
        } else {
            received.usage.clone()
        },
        String::new(),
    ]
}

fn line_break(input: &Interchange) -> Vec<u8> {
    input
        .header()
        .to_bytes()
        .into_iter()
        .skip_while(|&b| b != input.delimiters().segment)
        .skip(1)
        .collect()
}

fn finish(input: &Interchange, mut segments: Vec<Vec<String>>) -> Result<Interchange, AckError> {
    for segment in &mut segments[1..] {
        while segment.len() > 1 && segment.last().is_some_and(String::is_empty) {
            segment.pop();
        }
    }
    Ok(Interchange::from_elements(
        *input.delimiters(),
        &segments,
        &line_break(input),
    )?)
}

fn issues_between(
    issues: &[Issue],
    from: usize,
    to: Option<usize>,
) -> impl Iterator<Item = &Issue> {
    issues
        .iter()
        .filter(move |issue| issue.segment >= from && to.is_none_or(|to| issue.segment <= to))
}

/// Builds a `TA1` interchange acknowledgment for one interchange of
/// `input`. Without an explicit code and note, envelope issues of the
/// interchange reject it with note `001` (control numbers differ), `021`
/// (wrong group count), `023` (no `IEA` trailer) or `024` (invalid
/// content); otherwise it is accepted with note `000`.
pub fn build_ta1(input: &Interchange, options: &Ta1Options<'_>) -> Result<Interchange, AckError> {
    options.header.check()?;
    let received = envelope_of(input, options.interchange)?;
    let issues = input.validate();
    let derived =
        issues_between(&issues, received.isa, received.iea).find_map(|issue| match &issue.kind {
            IssueKind::ControlNumberMismatch { trailer: "IEA", .. } => Some("001"),
            IssueKind::CountMismatch { trailer: "IEA", .. } => Some("021"),
            IssueKind::MissingIea => Some("023"),
            IssueKind::UnexpectedSegment(_) => Some("024"),
            _ => None,
        });
    let code = options.code.unwrap_or(if derived.is_some() {
        AckCode::Rejected
    } else {
        AckCode::Accepted
    });
    let note = options.note.or(derived).unwrap_or("000");
    let control = format!("{:09}", options.header.control_number);
    let segments = vec![
        isa(input, &received, &options.header),
        vec![
            "TA1".into(),
            pad(&received.control_number, 9),
            received.date.clone(),
            received.time.clone(),
            code.as_str().into(),
            note.into(),
        ],
        vec!["IEA".into(), "0".into(), control],
    ];
    finish(input, segments)
}

fn derived_transaction(index: usize, group: &GroupEnvelope, issues: &[Issue]) -> TransactionAck {
    let transaction = &group.transactions[index];
    let mut syntax_errors = Vec::new();
    for issue in issues_between(issues, transaction.st, transaction.se) {
        let code = match &issue.kind {
            IssueKind::MissingSe if issue.segment == transaction.st => "2",
            IssueKind::ControlNumberMismatch { trailer: "SE", .. } => "3",
            IssueKind::CountMismatch { trailer: "SE", .. } => "4",
            _ => continue,
        };
        syntax_errors.push(code.to_owned());
    }
    TransactionAck {
        transaction: index,
        code: if syntax_errors.is_empty() {
            AckCode::Accepted
        } else {
            AckCode::Rejected
        },
        syntax_errors,
        segments: Vec::new(),
    }
}

/// Builds a `997` or `999` acknowledgment for one functional group of
/// `input`.
///
/// Every transaction set of the group gets an `AK2` loop. Envelope issues
/// reject a transaction set with `AK5`/`IK5` syntax code `2` (no `SE`),
/// `3` (control numbers differ) or `4` (wrong segment count), and are
/// reported for the group with `AK9` codes `3` (no `GE`), `4` (control
/// numbers differ) or `5` (wrong transaction set count). `AK9-01` is `A`
/// when everything was accepted, `E` when everything was accepted with
/// errors noted, `P` when some transaction sets were rejected and `R` when
/// all were.
pub fn build_functional_ack(
    input: &Interchange,
    options: &FunctionalAckOptions<'_>,
) -> Result<Interchange, AckError> {
    options.header.check()?;
    let received = envelope_of(input, options.interchange)?;
    let group = received
        .groups
        .get(options.group)
        .ok_or(AckError::NoGroup(options.group))?;
    let issues = input.validate();
    let is_999 = options.kind == FunctionalAckKind::Ack999;
    let (id, version) = if is_999 {
        ("999", "005010X231A1".to_owned())
    } else {
        ("997", group.version.clone())
    };
    let prefix = |ak: &str, ik: &str| if is_999 { ik.to_owned() } else { ak.to_owned() };

    let mut body: Vec<Vec<String>> = Vec::new();
    let mut ak1 = vec![
        "AK1".to_owned(),
        group.functional_id.clone(),
        group.control_number.clone(),
    ];
    if is_999 {
        ak1.push(group.version.clone());
    }
    body.push(ak1);
    let mut accepted = 0;
    let mut any_errors = false;
    for index in 0..group.transactions.len() {
        let result = options
            .transactions
            .iter()
            .find(|t| t.transaction == index)
            .cloned()
            .unwrap_or_else(|| derived_transaction(index, group, &issues));
        let transaction = &group.transactions[index];
        let mut ak2 = vec![
            "AK2".to_owned(),
            transaction.id.clone(),
            transaction.control_number.clone(),
        ];
        if is_999 && let Some(implementation) = &transaction.implementation {
            ak2.push(implementation.clone());
        }
        body.push(ak2);
        for segment in &result.segments {
            body.push(vec![
                prefix("AK3", "IK3"),
                segment.segment_id.clone(),
                segment.position.to_string(),
                segment.loop_id.clone().unwrap_or_default(),
                segment.code.clone(),
            ]);
            for element in &segment.elements {
                let separator = char::from(input.delimiters().component);
                let mut position = element.position.to_string();
                if element.component.is_some() || (is_999 && element.repetition.is_some()) {
                    position.push(separator);
                    if let Some(component) = element.component {
                        position.push_str(&component.to_string());
                    }
                }
                if is_999 && let Some(repetition) = element.repetition {
                    position.push(separator);
                    position.push_str(&repetition.to_string());
                }
                body.push(vec![
                    prefix("AK4", "IK4"),
                    position,
                    element.reference.clone().unwrap_or_default(),
                    element.code.clone(),
                    element.bad_value.clone().unwrap_or_default(),
                ]);
            }
        }
        let mut ak5 = vec![prefix("AK5", "IK5"), result.code.as_str().to_owned()];
        ak5.extend(result.syntax_errors.iter().take(5).cloned());
        body.push(ak5);
        if result.code.is_accepted() {
            accepted += 1;
        }
        any_errors |= result.code != AckCode::Accepted;
    }
    let group_errors: Vec<String> = issues_between(&issues, group.gs, group.ge)
        .filter_map(|issue| match &issue.kind {
            IssueKind::MissingGe if issue.segment == group.gs => Some("3"),
            IssueKind::ControlNumberMismatch { trailer: "GE", .. } => Some("4"),
            IssueKind::CountMismatch { trailer: "GE", .. } => Some("5"),
            _ => None,
        })
        .map(str::to_owned)
        .collect();
    let received_count = group.transactions.len();
    let included = group
        .ge
        .and_then(|ge| input.segment_at(ge))
        .and_then(|ge| ge.trimmed(1))
        .filter(|n| n.parse::<u32>().is_ok())
        .unwrap_or_else(|| received_count.to_string());
    let code = if received_count == 0 || accepted == 0 {
        "R"
    } else if accepted < received_count {
        "P"
    } else if any_errors || !group_errors.is_empty() {
        "E"
    } else {
        "A"
    };
    let mut ak9 = vec![
        "AK9".to_owned(),
        code.to_owned(),
        included,
        received_count.to_string(),
        accepted.to_string(),
    ];
    ak9.extend(group_errors.into_iter().take(5));
    body.push(ak9);

    let group_control = options.group_control_number.to_string();
    let mut st = vec!["ST".to_owned(), id.to_owned(), "0001".to_owned()];
    if is_999 {
        st.push(version.clone());
    }
    let mut segments = vec![
        isa(input, &received, &options.header),
        vec![
            "GS".into(),
            "FA".into(),
            group.receiver.clone(),
            group.sender.clone(),
            options.header.date.to_owned(),
            options.header.time.to_owned(),
            group_control.clone(),
            "X".into(),
            version,
        ],
        st,
    ];
    let count = body.len() + 2;
    segments.extend(body);
    segments.push(vec!["SE".into(), count.to_string(), "0001".into()]);
    segments.push(vec!["GE".into(), "1".into(), group_control]);
    segments.push(vec![
        "IEA".into(),
        "1".into(),
        format!("{:09}", options.header.control_number),
    ]);
    finish(input, segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAIMS: &str = "ISA*00*          *00*          *ZZ*SUBMITTER      *ZZ*RECEIVER       *260929*1200*^*00501*000000031*1*T*:~
GS*HC*SUBMITTER*RECEIVER*20260929*1200*17*X*005010X222A1~
ST*837*0001*005010X222A1~
BHT*0019*00*REF1*20260929*1200*CH~
SE*3*0001~
ST*837*0002*005010X222A1~
BHT*0019*00*REF2*20260929*1200*CH~
SE*9*0002~
GE*2*17~
IEA*1*000000031~
";

    fn header() -> AckHeader<'static> {
        AckHeader {
            control_number: 900,
            date: "20260929",
            time: "1305",
        }
    }

    fn text(x: &Interchange) -> String {
        String::from_utf8(x.to_bytes()).unwrap()
    }

    #[test]
    fn builds_ta1() {
        let input = Interchange::parse(CLAIMS.as_bytes()).unwrap();
        let ack = build_ta1(
            &input,
            &Ta1Options {
                header: header(),
                interchange: 0,
                code: None,
                note: None,
            },
        )
        .unwrap();
        assert_eq!(
            text(&ack),
            "ISA*00*          *00*          *ZZ*RECEIVER       *ZZ*SUBMITTER      *260929*1305*^*00501*000000900*0*T*:~
TA1*000000031*260929*1200*A*000~
IEA*0*000000900~
"
        );
        assert!(ack.validate().is_empty());
        let truncated = &CLAIMS[..CLAIMS.len() - 17];
        let input = Interchange::parse(truncated.as_bytes()).unwrap();
        let ack = build_ta1(
            &input,
            &Ta1Options {
                header: header(),
                interchange: 0,
                code: None,
                note: None,
            },
        )
        .unwrap();
        assert!(text(&ack).contains("TA1*000000031*260929*1200*R*023~"));
        assert_eq!(
            build_ta1(
                &input,
                &Ta1Options {
                    header: header(),
                    interchange: 1,
                    code: None,
                    note: None
                }
            ),
            Err(AckError::NoInterchange(1))
        );
    }

    #[test]
    fn builds_999_with_derived_and_supplied_errors() {
        let input = Interchange::parse(CLAIMS.as_bytes()).unwrap();
        let ack = build_functional_ack(
            &input,
            &FunctionalAckOptions {
                kind: FunctionalAckKind::Ack999,
                header: header(),
                group_control_number: 5,
                interchange: 0,
                group: 0,
                transactions: vec![TransactionAck {
                    transaction: 0,
                    code: AckCode::AcceptedWithErrors,
                    syntax_errors: vec!["5".into()],
                    segments: vec![SegmentError {
                        segment_id: "BHT".into(),
                        position: 2,
                        loop_id: None,
                        code: "8".into(),
                        elements: vec![ElementError {
                            position: 6,
                            component: None,
                            repetition: None,
                            reference: Some("640".into()),
                            code: "7".into(),
                            bad_value: Some("CH".into()),
                        }],
                    }],
                }],
            },
        )
        .unwrap();
        assert_eq!(
            text(&ack),
            "ISA*00*          *00*          *ZZ*RECEIVER       *ZZ*SUBMITTER      *260929*1305*^*00501*000000900*0*T*:~
GS*FA*RECEIVER*SUBMITTER*20260929*1305*5*X*005010X231A1~
ST*999*0001*005010X231A1~
AK1*HC*17*005010X222A1~
AK2*837*0001*005010X222A1~
IK3*BHT*2**8~
IK4*6*640*7*CH~
IK5*E*5~
AK2*837*0002*005010X222A1~
IK5*R*4~
AK9*P*2*2*1~
SE*10*0001~
GE*1*5~
IEA*1*000000900~
"
        );
        assert!(ack.validate().is_empty(), "{:?}", ack.validate());
    }

    #[test]
    fn builds_997_for_older_versions() {
        let old = "ISA|00|          |00|          |ZZ|LAB            |ZZ|PAYER          |260929|1200|U|00401|000000001|0|P|>\nGS|HS|LAB|PAYER|20260929|1200|3|X|004010X092A1\nST|270|0001\nSE|2|0001\nGE|1|3\nIEA|1|000000001\n";
        let input = Interchange::parse(old.as_bytes()).unwrap();
        let ack = build_functional_ack(
            &input,
            &FunctionalAckOptions {
                kind: FunctionalAckKind::Ack997,
                header: header(),
                group_control_number: 1,
                interchange: 0,
                group: 0,
                transactions: Vec::new(),
            },
        )
        .unwrap();
        let text = text(&ack);
        assert!(text.contains("|U|00401|000000900|0|P|>\nGS|FA|PAYER|LAB|20260929|1305|1|X|004010X092A1\nST|997|0001\nAK1|HS|3\nAK2|270|0001\nAK5|A\nAK9|A|1|1|1\nSE|6|0001\n"), "{text}");
        assert!(matches!(
            build_functional_ack(
                &input,
                &FunctionalAckOptions {
                    kind: FunctionalAckKind::Ack997,
                    header: AckHeader {
                        date: "260929",
                        ..header()
                    },
                    group_control_number: 1,
                    interchange: 0,
                    group: 0,
                    transactions: Vec::new(),
                }
            ),
            Err(AckError::InvalidDateTime(_))
        ));
    }
}
