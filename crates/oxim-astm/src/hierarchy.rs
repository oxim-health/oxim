//! The patient → order → result hierarchy of an ASTM E1394 message.

use crate::message::Message;
use crate::record::RecordRef;

/// A patient record with its comments and orders.
#[derive(Debug, Clone)]
pub struct PatientGroup<'a> {
    /// The patient (`P`) record, or `None` for orders sent without one.
    pub patient: Option<RecordRef<'a>>,
    /// Comment (`C`) records that follow the patient record.
    pub comments: Vec<RecordRef<'a>>,
    /// The orders of this patient.
    pub orders: Vec<OrderGroup<'a>>,
}

/// An order record with its comments and results.
#[derive(Debug, Clone)]
pub struct OrderGroup<'a> {
    /// The order (`O`) record, or `None` for results sent without one.
    pub order: Option<RecordRef<'a>>,
    /// Comment (`C`) records that follow the order record.
    pub comments: Vec<RecordRef<'a>>,
    /// The results of this order.
    pub results: Vec<ResultGroup<'a>>,
}

/// A result record with its comments.
#[derive(Debug, Clone)]
pub struct ResultGroup<'a> {
    /// The result (`R`) record.
    pub result: RecordRef<'a>,
    /// Comment (`C`) records that follow the result record.
    pub comments: Vec<RecordRef<'a>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    None,
    Patient,
    Order,
    Result,
}

impl Message {
    /// Groups the records into patients, orders, results and their
    /// comments, following record order as E1394 prescribes.
    ///
    /// Orders before any patient record and results before any order record
    /// are placed in groups whose `patient` or `order` is `None`. Comments
    /// before the first patient record, and request (`Q`), manufacturer
    /// (`M`) and scientific (`S`) records are not part of the hierarchy; use
    /// [`Message::records_of_type`] for them.
    pub fn patients(&self) -> Vec<PatientGroup<'_>> {
        let mut patients: Vec<PatientGroup<'_>> = Vec::new();
        let mut level = Level::None;
        for record in self.records() {
            match record.record_type() {
                b"P" => {
                    patients.push(PatientGroup {
                        patient: Some(record),
                        comments: Vec::new(),
                        orders: Vec::new(),
                    });
                    level = Level::Patient;
                }
                b"O" => {
                    current_patient(&mut patients).orders.push(OrderGroup {
                        order: Some(record),
                        comments: Vec::new(),
                        results: Vec::new(),
                    });
                    level = Level::Order;
                }
                b"R" => {
                    current_order(&mut patients).results.push(ResultGroup {
                        result: record,
                        comments: Vec::new(),
                    });
                    level = Level::Result;
                }
                b"C" => match level {
                    Level::None => {}
                    Level::Patient => current_patient(&mut patients).comments.push(record),
                    Level::Order => current_order(&mut patients).comments.push(record),
                    Level::Result => {
                        if let Some(result) = current_order(&mut patients).results.last_mut() {
                            result.comments.push(record);
                        }
                    }
                },
                b"H" | b"L" => level = Level::None,
                _ => {}
            }
        }
        patients
    }
}

fn current_patient<'a, 'b>(patients: &'b mut Vec<PatientGroup<'a>>) -> &'b mut PatientGroup<'a> {
    if patients.is_empty() {
        patients.push(PatientGroup {
            patient: None,
            comments: Vec::new(),
            orders: Vec::new(),
        });
    }
    let last = patients.len() - 1;
    &mut patients[last]
}

fn current_order<'a, 'b>(patients: &'b mut Vec<PatientGroup<'a>>) -> &'b mut OrderGroup<'a> {
    let patient = current_patient(patients);
    if patient.orders.is_empty() {
        patient.orders.push(OrderGroup {
            order: None,
            comments: Vec::new(),
            results: Vec::new(),
        });
    }
    let last = patient.orders.len() - 1;
    &mut patient.orders[last]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_records() {
        let message = Message::parse(
            b"H|\\^&\rP|1\rC|1|I|patient note\rO|1|S1\rR|1|^^^GLU|5.4\rC|1|I|result note\r\
R|2|^^^HGB|13.2\rO|2|S2\rC|1|I|order note\rP|2\rO|1|S3\rR|1|^^^K|4.1\rL|1\r",
        )
        .unwrap();
        let patients = message.patients();
        assert_eq!(patients.len(), 2);
        let first = &patients[0];
        assert_eq!(first.comments.len(), 1);
        assert_eq!(first.orders.len(), 2);
        assert_eq!(first.orders[0].results.len(), 2);
        assert_eq!(first.orders[0].results[0].comments.len(), 1);
        assert_eq!(
            first.orders[0].results[0].comments[0].get("4").unwrap(),
            "result note"
        );
        assert_eq!(first.orders[1].comments[0].get("4").unwrap(), "order note");
        assert_eq!(
            patients[1].orders[0].results[0].result.get("4").unwrap(),
            "4.1"
        );
    }

    #[test]
    fn tolerates_missing_parents() {
        let message = Message::parse(b"H|\\^&\rR|1|^^^GLU|5.4\rL|1\r").unwrap();
        let patients = message.patients();
        assert_eq!(patients.len(), 1);
        assert!(patients[0].patient.is_none());
        assert!(patients[0].orders[0].order.is_none());
        assert_eq!(patients[0].orders[0].results.len(), 1);
    }
}
