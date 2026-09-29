use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::id::{ChannelId, ConnectorId, DeviceId, MessageId};
use crate::time::Timestamp;

/// The wire format of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DataType {
    /// HL7 v2.x in ER7 (pipe-delimited) encoding.
    #[serde(rename = "hl7v2")]
    Hl7V2,
    /// ASTM E1394 / CLSI LIS02 records.
    #[serde(rename = "astm")]
    Astm,
    /// CLSI POCT1-A XML.
    #[serde(rename = "poct1a")]
    Poct1a,
    /// HL7 FHIR resources in JSON or XML.
    #[serde(rename = "fhir")]
    Fhir,
    /// HL7 CDA R2 documents.
    #[serde(rename = "cda")]
    Cda,
    /// DICOM objects.
    #[serde(rename = "dicom")]
    Dicom,
    /// ASC X12 EDI.
    #[serde(rename = "x12")]
    X12,
    /// NCPDP pharmacy messages.
    #[serde(rename = "ncpdp")]
    Ncpdp,
    /// Generic JSON.
    #[serde(rename = "json")]
    Json,
    /// Generic XML.
    #[serde(rename = "xml")]
    Xml,
    /// Delimited text such as CSV.
    #[serde(rename = "delimited")]
    Delimited,
    /// Fixed-width text records.
    #[serde(rename = "fixed_width")]
    FixedWidth,
    /// Uninterpreted bytes.
    #[serde(rename = "raw")]
    Raw,
}

impl DataType {
    /// Every data type, in declaration order.
    pub const ALL: [Self; 13] = [
        Self::Hl7V2,
        Self::Astm,
        Self::Poct1a,
        Self::Fhir,
        Self::Cda,
        Self::Dicom,
        Self::X12,
        Self::Ncpdp,
        Self::Json,
        Self::Xml,
        Self::Delimited,
        Self::FixedWidth,
        Self::Raw,
    ];

    /// The configuration name, for example `hl7v2`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hl7V2 => "hl7v2",
            Self::Astm => "astm",
            Self::Poct1a => "poct1a",
            Self::Fhir => "fhir",
            Self::Cda => "cda",
            Self::Dicom => "dicom",
            Self::X12 => "x12",
            Self::Ncpdp => "ncpdp",
            Self::Json => "json",
            Self::Xml => "xml",
            Self::Delimited => "delimited",
            Self::FixedWidth => "fixed_width",
            Self::Raw => "raw",
        }
    }
}

/// Returned when a data type name is unknown.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown data type {0:?}")]
pub struct UnknownDataType(pub String);

impl FromStr for DataType {
    type Err = UnknownDataType;

    fn from_str(s: &str) -> Result<Self, UnknownDataType> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| UnknownDataType(s.to_owned()))
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A message as received by a source connector, before any processing.
///
/// The raw bytes are kept exactly as received; every later stage (parsed,
/// transformed, encoded) is derived from them and stored separately, so the
/// original can always be reprocessed (ADR 0007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Unique, time-ordered identifier.
    pub id: MessageId,
    /// The channel that received the message.
    pub channel: ChannelId,
    /// The source connector that received it.
    pub connector: ConnectorId,
    /// When the source connector received the last byte.
    pub received_at: Timestamp,
    /// The declared data type of `raw`.
    pub data_type: DataType,
    /// The bytes exactly as received, without transport framing.
    pub raw: Vec<u8>,
    /// The remote peer: network address, serial port or file path.
    pub peer: Option<String>,
    /// The registered device the message came from, if identified.
    pub device: Option<DeviceId>,
    /// An identifier linking related messages, such as a query and its
    /// response.
    pub correlation_id: Option<String>,
    /// Free-form metadata set by connectors and scripts.
    pub metadata: BTreeMap<String, String>,
}

impl Envelope {
    /// Creates an envelope with no peer, device, correlation or metadata.
    pub fn new(
        id: MessageId,
        channel: ChannelId,
        connector: ConnectorId,
        received_at: Timestamp,
        data_type: DataType,
        raw: Vec<u8>,
    ) -> Self {
        Self {
            id,
            channel,
            connector,
            received_at,
            data_type,
            raw,
            peer: None,
            device: None,
            correlation_id: None,
            metadata: BTreeMap::new(),
        }
    }
}

/// Where a message is in its channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    /// Stored and acknowledged, not processed yet.
    Received,
    /// Rejected by a channel filter; nothing will be sent.
    Filtered,
    /// Transformed and handed to the destination queues.
    Transformed,
    /// Every destination reached a final state.
    Completed,
    /// Processing failed before the destinations; can be reprocessed.
    Error,
}

/// Where a message is for one destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationStatus {
    /// Waiting in the destination queue.
    Queued,
    /// Being sent.
    Sending,
    /// Delivered and, where the protocol has one, acknowledged.
    Sent,
    /// Skipped by a destination filter.
    Filtered,
    /// A delivery attempt failed; another will be made.
    Retrying,
    /// Delivery gave up; waits for an operator to reprocess it.
    Failed,
}

impl DestinationStatus {
    /// Whether no further delivery attempt will be made automatically.
    pub fn is_final(self) -> bool {
        matches!(self, Self::Sent | Self::Filtered | Self::Failed)
    }
}

/// Returned when a status name is unknown.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown status {0:?}")]
pub struct UnknownStatus(pub String);

macro_rules! status_names {
    ($type:ty { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $type {
            /// Every status, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The stored and serialized name.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }

        impl FromStr for $type {
            type Err = UnknownStatus;

            fn from_str(s: &str) -> Result<Self, UnknownStatus> {
                match s {
                    $($name => Ok(Self::$variant),)+
                    _ => Err(UnknownStatus(s.to_owned())),
                }
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

status_names!(MessageStatus {
    Received => "received",
    Filtered => "filtered",
    Transformed => "transformed",
    Completed => "completed",
    Error => "error",
});

status_names!(DestinationStatus {
    Queued => "queued",
    Sending => "sending",
    Sent => "sent",
    Filtered => "filtered",
    Retrying => "retrying",
    Failed => "failed",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_type_names_round_trip() {
        for kind in DataType::ALL {
            assert_eq!(kind.as_str().parse::<DataType>().unwrap(), kind);
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        assert!("HL7".parse::<DataType>().is_err());
    }

    #[test]
    fn status_names_match_serde() {
        for status in MessageStatus::ALL {
            let json = serde_json::to_string(status).unwrap();
            assert_eq!(json, format!("\"{status}\""));
            assert_eq!(status.as_str().parse::<MessageStatus>().unwrap(), *status);
        }
        for status in DestinationStatus::ALL {
            let json = serde_json::to_string(status).unwrap();
            assert_eq!(json, format!("\"{status}\""));
            assert_eq!(
                status.as_str().parse::<DestinationStatus>().unwrap(),
                *status
            );
        }
        assert!("sending!".parse::<DestinationStatus>().is_err());
    }

    #[test]
    fn final_destination_states() {
        assert!(DestinationStatus::Sent.is_final());
        assert!(DestinationStatus::Failed.is_final());
        assert!(!DestinationStatus::Retrying.is_final());
    }
}
