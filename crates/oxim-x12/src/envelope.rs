//! The envelope structure (`ISA`/`GS`/`ST` … `SE`/`GE`/`IEA`) and its
//! validation.

use std::fmt;

use crate::interchange::Interchange;
use crate::segment::SegmentRef;

/// The envelopes of every interchange in the input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    /// Interchanges in order.
    pub interchanges: Vec<InterchangeEnvelope>,
}

/// One `ISA` … `IEA` interchange.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterchangeEnvelope {
    /// Index of the `ISA` segment.
    pub isa: usize,
    /// Index of the `IEA` segment, if present.
    pub iea: Option<usize>,
    /// ISA-05 and ISA-06: sender qualifier and identifier, without padding.
    pub sender: (String, String),
    /// ISA-07 and ISA-08: receiver qualifier and identifier.
    pub receiver: (String, String),
    /// ISA-09 (YYMMDD).
    pub date: String,
    /// ISA-10 (HHMM).
    pub time: String,
    /// ISA-12, for example `00501`.
    pub version: String,
    /// ISA-13.
    pub control_number: String,
    /// ISA-14: whether a TA1 acknowledgment is requested (`1`).
    pub acknowledgment_requested: bool,
    /// ISA-15: `P` production, `T` test.
    pub usage: String,
    /// Functional groups in order.
    pub groups: Vec<GroupEnvelope>,
}

/// One `GS` … `GE` functional group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupEnvelope {
    /// Index of the `GS` segment.
    pub gs: usize,
    /// Index of the `GE` segment, if present.
    pub ge: Option<usize>,
    /// GS-01, for example `HC` (claims) or `HS` (eligibility inquiries).
    pub functional_id: String,
    /// GS-02.
    pub sender: String,
    /// GS-03.
    pub receiver: String,
    /// GS-04 (CCYYMMDD).
    pub date: String,
    /// GS-05.
    pub time: String,
    /// GS-06.
    pub control_number: String,
    /// GS-08, for example `005010X222A1`.
    pub version: String,
    /// Transaction sets in order.
    pub transactions: Vec<TransactionEnvelope>,
}

/// One `ST` … `SE` transaction set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransactionEnvelope {
    /// Index of the `ST` segment.
    pub st: usize,
    /// Index of the `SE` segment, if present.
    pub se: Option<usize>,
    /// ST-01, for example `837`.
    pub id: String,
    /// ST-02.
    pub control_number: String,
    /// ST-03, the implementation convention reference, if present.
    pub implementation: Option<String>,
}

/// An envelope problem found by [`Interchange::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Issue {
    /// Index of the segment the issue refers to.
    pub segment: usize,
    /// What is wrong.
    pub kind: IssueKind,
}

/// The kinds of envelope problems.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IssueKind {
    /// An interchange has no `IEA` trailer.
    MissingIea,
    /// A functional group has no `GE` trailer.
    MissingGe,
    /// A transaction set has no `SE` trailer.
    MissingSe,
    /// A trailer without its header, or a segment outside a transaction
    /// set.
    UnexpectedSegment(String),
    /// A trailer's control number differs from its header's.
    ControlNumberMismatch {
        /// The trailer, `IEA`, `GE` or `SE`.
        trailer: &'static str,
        /// The header's control number.
        header: String,
        /// The trailer's control number.
        found: String,
    },
    /// A trailer's count differs from the actual count.
    CountMismatch {
        /// The trailer, `IEA`, `GE` or `SE`.
        trailer: &'static str,
        /// The declared count.
        declared: String,
        /// The actual count.
        actual: usize,
    },
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "segment {}: ", self.segment + 1)?;
        match &self.kind {
            IssueKind::MissingIea => f.write_str("interchange has no IEA trailer"),
            IssueKind::MissingGe => f.write_str("functional group has no GE trailer"),
            IssueKind::MissingSe => f.write_str("transaction set has no SE trailer"),
            IssueKind::UnexpectedSegment(id) => write!(f, "unexpected segment {id}"),
            IssueKind::ControlNumberMismatch {
                trailer,
                header,
                found,
            } => write!(
                f,
                "{trailer} control number {found:?} differs from the header's {header:?}"
            ),
            IssueKind::CountMismatch {
                trailer,
                declared,
                actual,
            } => write!(f, "{trailer} declares {declared:?} but {actual} were found"),
        }
    }
}

fn same_number(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn count_matches(declared: &str, actual: usize) -> bool {
    declared
        .trim()
        .parse::<usize>()
        .is_ok_and(|declared| declared == actual)
}

struct Walker {
    envelope: Envelope,
    issues: Vec<Issue>,
    in_group: bool,
    in_transaction: bool,
}

impl Walker {
    fn issue(&mut self, segment: usize, kind: IssueKind) {
        self.issues.push(Issue { segment, kind });
    }

    fn interchange(&mut self) -> Option<&mut InterchangeEnvelope> {
        self.envelope.interchanges.last_mut()
    }

    fn group(&mut self) -> Option<&mut GroupEnvelope> {
        self.interchange()?.groups.last_mut()
    }

    fn close_transaction(&mut self, at: usize) {
        if self.in_transaction {
            let st = self
                .group()
                .and_then(|g| g.transactions.last())
                .map_or(at, |t| t.st);
            self.issue(st, IssueKind::MissingSe);
            self.in_transaction = false;
        }
    }

    fn close_group(&mut self, at: usize) {
        self.close_transaction(at);
        if self.in_group {
            let gs = self.group().map_or(at, |g| g.gs);
            self.issue(gs, IssueKind::MissingGe);
            self.in_group = false;
        }
    }

    fn close_interchange(&mut self, at: usize) {
        self.close_group(at);
        if let Some(isa) = self
            .envelope
            .interchanges
            .last()
            .filter(|i| i.iea.is_none())
            .map(|i| i.isa)
        {
            self.issue(isa, IssueKind::MissingIea);
        }
    }

    fn open_interchange(&self) -> bool {
        self.envelope
            .interchanges
            .last()
            .is_some_and(|i| i.iea.is_none())
    }

    fn visit(&mut self, segment: &SegmentRef<'_>) {
        let at = segment.index();
        let text = |n| segment.trimmed(n).unwrap_or_default();
        match segment.id() {
            b"ISA" => {
                if self.open_interchange() {
                    self.close_interchange(at);
                }
                self.envelope.interchanges.push(InterchangeEnvelope {
                    isa: at,
                    iea: None,
                    sender: (text(5), text(6)),
                    receiver: (text(7), text(8)),
                    date: text(9),
                    time: text(10),
                    version: text(12),
                    control_number: text(13),
                    acknowledgment_requested: text(14) == "1",
                    usage: text(15),
                    groups: Vec::new(),
                });
            }
            b"GS" if self.open_interchange() => {
                self.close_group(at);
                let group = GroupEnvelope {
                    gs: at,
                    ge: None,
                    functional_id: text(1),
                    sender: text(2),
                    receiver: text(3),
                    date: text(4),
                    time: text(5),
                    control_number: text(6),
                    version: text(8),
                    transactions: Vec::new(),
                };
                if let Some(interchange) = self.interchange() {
                    interchange.groups.push(group);
                }
                self.in_group = true;
            }
            b"ST" if self.in_group => {
                self.close_transaction(at);
                let transaction = TransactionEnvelope {
                    st: at,
                    se: None,
                    id: text(1),
                    control_number: text(2),
                    implementation: segment.trimmed(3),
                };
                if let Some(group) = self.group() {
                    group.transactions.push(transaction);
                }
                self.in_transaction = true;
            }
            b"SE" if self.in_transaction => {
                self.in_transaction = false;
                let Some(transaction) = self.group().and_then(|g| g.transactions.last_mut()) else {
                    return;
                };
                transaction.se = Some(at);
                let (st, control) = (transaction.st, transaction.control_number.clone());
                let actual = at - st + 1;
                if !count_matches(&text(1), actual) {
                    self.issue(
                        at,
                        IssueKind::CountMismatch {
                            trailer: "SE",
                            declared: text(1),
                            actual,
                        },
                    );
                }
                if !same_number(&text(2), &control) {
                    self.issue(
                        at,
                        IssueKind::ControlNumberMismatch {
                            trailer: "SE",
                            header: control,
                            found: text(2),
                        },
                    );
                }
            }
            b"GE" if self.in_group => {
                self.close_transaction(at);
                self.in_group = false;
                let Some(group) = self.group() else {
                    return;
                };
                group.ge = Some(at);
                let (actual, control) = (group.transactions.len(), group.control_number.clone());
                if !count_matches(&text(1), actual) {
                    self.issue(
                        at,
                        IssueKind::CountMismatch {
                            trailer: "GE",
                            declared: text(1),
                            actual,
                        },
                    );
                }
                if !same_number(&text(2), &control) {
                    self.issue(
                        at,
                        IssueKind::ControlNumberMismatch {
                            trailer: "GE",
                            header: control,
                            found: text(2),
                        },
                    );
                }
            }
            b"IEA" if self.open_interchange() => {
                self.close_group(at);
                let Some(interchange) = self.interchange() else {
                    return;
                };
                interchange.iea = Some(at);
                let (actual, control) =
                    (interchange.groups.len(), interchange.control_number.clone());
                if !count_matches(&text(1), actual) {
                    self.issue(
                        at,
                        IssueKind::CountMismatch {
                            trailer: "IEA",
                            declared: text(1),
                            actual,
                        },
                    );
                }
                if !same_number(&text(2), &control) {
                    self.issue(
                        at,
                        IssueKind::ControlNumberMismatch {
                            trailer: "IEA",
                            header: control,
                            found: text(2),
                        },
                    );
                }
            }
            // An interchange acknowledgment sits between ISA and GS.
            b"TA1" if self.open_interchange() && !self.in_group => {}
            // Blank segments, such as trailing line noise, are ignored.
            b"" => {}
            _ if self.in_transaction => {}
            id => {
                let id = String::from_utf8_lossy(id).into_owned();
                self.issue(at, IssueKind::UnexpectedSegment(id));
            }
        }
    }
}

impl Envelope {
    pub(crate) fn of(interchange: &Interchange) -> (Self, Vec<Issue>) {
        let mut walker = Walker {
            envelope: Self::default(),
            issues: Vec::new(),
            in_group: false,
            in_transaction: false,
        };
        for segment in interchange.segments() {
            walker.visit(&segment);
        }
        let end = interchange.segment_count();
        if walker.open_interchange() {
            walker.close_interchange(end);
        }
        let mut issues = walker.issues;
        issues.sort_by_key(|issue| issue.segment);
        (walker.envelope, issues)
    }

    /// Every transaction set with its group and interchange positions:
    /// `(interchange, group, transaction)`.
    pub fn transactions(&self) -> impl Iterator<Item = (usize, usize, &TransactionEnvelope)> + '_ {
        self.interchanges
            .iter()
            .enumerate()
            .flat_map(|(i, interchange)| {
                interchange
                    .groups
                    .iter()
                    .enumerate()
                    .flat_map(move |(g, group)| group.transactions.iter().map(move |t| (i, g, t)))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interchange::Interchange;

    fn issues(text: &str) -> Vec<IssueKind> {
        Interchange::parse(text.as_bytes())
            .unwrap()
            .validate()
            .into_iter()
            .map(|issue| issue.kind)
            .collect()
    }

    const ISA: &str = "ISA*00*          *00*          *ZZ*A              *ZZ*B              *260929*1200*^*00501*000000009*1*P*:~";

    #[test]
    fn describes_the_envelope() {
        let text = format!(
            "{ISA}GS*HS*A*B*20260929*1200*7*X*005010X279A1~ST*270*0001*005010X279A1~BHT*0022*13~SE*3*0001~ST*270*0002~SE*2*0002~GE*2*7~IEA*1*000000009~"
        );
        let x = Interchange::parse(text.as_bytes()).unwrap();
        let envelope = x.envelope();
        let interchange = &envelope.interchanges[0];
        assert_eq!(interchange.sender, ("ZZ".into(), "A".into()));
        assert_eq!(interchange.control_number, "000000009");
        assert!(interchange.acknowledgment_requested);
        let group = &interchange.groups[0];
        assert_eq!((group.functional_id.as_str(), group.ge), ("HS", Some(7)));
        assert_eq!(
            group.transactions[0].implementation.as_deref(),
            Some("005010X279A1")
        );
        assert_eq!(group.transactions[1].implementation, None);
        assert_eq!(envelope.transactions().count(), 2);
        assert!(x.validate().is_empty());
    }

    #[test]
    fn reports_counts_control_numbers_and_missing_trailers() {
        let bad = format!(
            "{ISA}GS*HS*A*B*20260929*1200*7*X*005010X279A1~ST*270*0001~BHT*0022~SE*9*0002~GE*3*8~IEA*2*000000001~"
        );
        let kinds = issues(&bad);
        assert_eq!(kinds.len(), 6, "{kinds:?}");
        assert!(kinds.contains(&IssueKind::CountMismatch {
            trailer: "SE",
            declared: "9".into(),
            actual: 3
        }));
        assert!(kinds.contains(&IssueKind::ControlNumberMismatch {
            trailer: "IEA",
            header: "000000009".into(),
            found: "000000001".into()
        }));
        let open = format!("{ISA}GS*HS*A*B*20260929*1200*7*X*005010X279A1~ST*270*0001~BHT*0022~");
        assert_eq!(
            issues(&open),
            [
                IssueKind::MissingIea,
                IssueKind::MissingGe,
                IssueKind::MissingSe
            ]
        );
        let stray = format!("{ISA}BHT*0022~GE*1*1~IEA*0*000000009~");
        assert_eq!(
            issues(&stray),
            [
                IssueKind::UnexpectedSegment("BHT".into()),
                IssueKind::UnexpectedSegment("GE".into())
            ]
        );
        let ta1 = format!("{ISA}TA1*000000001*260929*1200*A*000~IEA*0*000000009~");
        assert!(issues(&ta1).is_empty());
    }

    #[test]
    fn accepts_numerically_equal_control_numbers() {
        assert!(same_number("0001", "1"));
        assert!(!same_number("A1", "1"));
        assert!(same_number(" X ", "X"));
    }
}
