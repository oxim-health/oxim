//! Responses to billing and other request transmissions.

use thiserror::Error;

use crate::error::PathError;
use crate::header::HeaderKind;
use crate::transmission::Transmission;

/// Returned when a response cannot be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ResponseError {
    /// The transmission being answered is itself a response.
    #[error("only request transmissions can be answered")]
    NotARequest,
    /// The number of transaction responses does not match the request.
    #[error("the request has {request} transactions but {response} responses were given")]
    TransactionCount {
        /// Transactions in the request.
        request: usize,
        /// Responses given.
        response: usize,
    },
    /// More transactions than the one-digit header count can express.
    #[error("a transmission holds at most 9 transactions")]
    TooManyTransactions,
    /// A value cannot be written.
    #[error(transparent)]
    Value(#[from] PathError),
}

/// Header Response Status (501-F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeaderStatus {
    /// `A`: the transmission was accepted; see each transaction's status.
    Accepted,
    /// `R`: the whole transmission was rejected.
    Rejected,
}

/// Transaction Response Status (112-AN).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransactionStatus {
    /// `P`: paid.
    Paid,
    /// `D`: duplicate of a paid claim.
    DuplicateOfPaid,
    /// `C`: captured.
    Captured,
    /// `Q`: duplicate of a captured claim.
    DuplicateOfCaptured,
    /// `A`: approved (non-billing transactions).
    Approved,
    /// `S`: duplicate of an approved transaction.
    DuplicateOfApproved,
    /// `R`: rejected.
    Rejected,
}

impl TransactionStatus {
    /// The status code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paid => "P",
            Self::DuplicateOfPaid => "D",
            Self::Captured => "C",
            Self::DuplicateOfCaptured => "Q",
            Self::Approved => "A",
            Self::DuplicateOfApproved => "S",
            Self::Rejected => "R",
        }
    }
}

/// The response to one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionResponse {
    /// 112-AN.
    pub status: TransactionStatus,
    /// 503-F3, the authorization number.
    pub authorization_number: Option<String>,
    /// 511-FB reject codes; 510-FA carries their count.
    pub reject_codes: Vec<String>,
    /// 526-FQ additional message information (qualifier 132-UH `01`).
    pub message: Option<String>,
}

/// Options for [`build_response`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseOptions {
    /// 501-F1.
    pub status: HeaderStatus,
    /// 504-F4 in a Response Message segment (`AM20`).
    pub message: Option<String>,
    /// One response per request transaction, in order. May be empty when
    /// the header is rejected.
    pub transactions: Vec<TransactionResponse>,
}

/// Builds the response to a request transmission.
///
/// The response header copies the version, transaction code, service
/// provider and date of service of the request. Each transaction group
/// gets a Response Status segment (`AM21`) and, when the request
/// transaction has a Claim segment (`AM07`), a Response Claim segment
/// (`AM22`) echoing the prescription/service reference number.
pub fn build_response(
    request: &Transmission,
    options: &ResponseOptions,
) -> Result<Transmission, ResponseError> {
    if request.kind() != HeaderKind::Request {
        return Err(ResponseError::NotARequest);
    }
    let request_count = request.transaction_count();
    let header_rejected = options.status == HeaderStatus::Rejected;
    if options.transactions.len() != request_count
        && !(header_rejected && options.transactions.is_empty())
    {
        return Err(ResponseError::TransactionCount {
            request: request_count,
            response: options.transactions.len(),
        });
    }
    if options.transactions.len() > 9 {
        return Err(ResponseError::TooManyTransactions);
    }
    let mut response = Transmission::new(HeaderKind::Response);
    for id in ["A2", "A3", "B2", "B1", "D1"] {
        if let Some(value) = request.header_value(id) {
            response.set_header(id, &value)?;
        }
    }
    response.set_header("A9", &options.transactions.len().to_string())?;
    response.set_header("F1", if header_rejected { "R" } else { "A" })?;
    if let Some(message) = &options.message {
        response.push_segment("AM20", &[("F4", message)])?;
    }
    for (index, transaction) in options.transactions.iter().enumerate() {
        response.push_group();
        let count = transaction.reject_codes.len().to_string();
        let mut fields: Vec<(&str, &str)> = vec![("AN", transaction.status.as_str())];
        if let Some(authorization) = &transaction.authorization_number {
            fields.push(("F3", authorization));
        }
        if !transaction.reject_codes.is_empty() {
            fields.push(("FA", &count));
            for code in &transaction.reject_codes {
                fields.push(("FB", code));
            }
        }
        if let Some(message) = &transaction.message {
            fields.push(("UF", "1"));
            fields.push(("UH", "01"));
            fields.push(("FQ", message));
        }
        response.push_segment("AM21", &fields)?;
        let claim = request
            .segments_of(Some(index))
            .find(|segment| segment.id().as_deref() == Some("AM07"));
        if let Some(claim) = claim {
            let qualifier = claim.field("EM", 1).unwrap_or_default().into_owned();
            let reference = claim.field("D2", 1).unwrap_or_default().into_owned();
            response.push_segment("AM22", &[("EM", &qualifier), ("D2", &reference)])?;
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transmission::tests::claim;

    #[test]
    fn answers_a_claim() {
        let request = Transmission::parse(&claim()).unwrap();
        let paid = build_response(
            &request,
            &ResponseOptions {
                status: HeaderStatus::Accepted,
                message: Some("SYNTHETIC TEST PROCESSOR".into()),
                transactions: vec![TransactionResponse {
                    status: TransactionStatus::Paid,
                    authorization_number: Some("AUTH0001".into()),
                    reject_codes: Vec::new(),
                    message: None,
                }],
            },
        )
        .unwrap();
        let bytes = paid.to_bytes();
        assert_eq!(&bytes[..31], b"D0B11A011234567893     20260929");
        let reparsed = Transmission::parse(&bytes).unwrap();
        assert_eq!(reparsed.kind(), HeaderKind::Response);
        assert_eq!(
            reparsed.get("AM20.F4").as_deref(),
            Some("SYNTHETIC TEST PROCESSOR")
        );
        assert_eq!(reparsed.get("AM21.AN").as_deref(), Some("P"));
        assert_eq!(reparsed.get("AM22.D2").as_deref(), Some("000000123456"));
        assert!(reparsed.validate().is_empty());

        let rejected = build_response(
            &request,
            &ResponseOptions {
                status: HeaderStatus::Accepted,
                message: None,
                transactions: vec![TransactionResponse {
                    status: TransactionStatus::Rejected,
                    authorization_number: None,
                    reject_codes: vec!["65".into(), "M4".into()],
                    message: Some("SYNTHETIC REJECT".into()),
                }],
            },
        )
        .unwrap();
        assert_eq!(rejected.get("AM21.FA").as_deref(), Some("2"));
        assert_eq!(rejected.get("AM21.FB[2]").as_deref(), Some("M4"));
        assert_eq!(rejected.get("AM21.FQ").as_deref(), Some("SYNTHETIC REJECT"));
    }

    #[test]
    fn checks_the_transaction_count() {
        let request = Transmission::parse(&claim()).unwrap();
        let options = ResponseOptions {
            status: HeaderStatus::Accepted,
            message: None,
            transactions: Vec::new(),
        };
        assert_eq!(
            build_response(&request, &options),
            Err(ResponseError::TransactionCount {
                request: 1,
                response: 0
            })
        );
        let header_rejected = build_response(
            &request,
            &ResponseOptions {
                status: HeaderStatus::Rejected,
                ..options
            },
        )
        .unwrap();
        assert_eq!(header_rejected.header_value("F1").as_deref(), Some("R"));
        assert_eq!(
            build_response(
                &header_rejected,
                &ResponseOptions {
                    status: HeaderStatus::Rejected,
                    message: None,
                    transactions: Vec::new()
                }
            ),
            Err(ResponseError::NotARequest)
        );
    }
}
