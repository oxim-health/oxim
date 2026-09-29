//! DIMSE command sets (PS3.7): the composite (C-ECHO, C-STORE, C-FIND,
//! C-MOVE, C-GET) and normalized (N-CREATE, N-SET, N-ACTION,
//! N-EVENT-REPORT) messages OXIM exchanges, encoded in Implicit VR Little
//! Endian as PS3.7 requires.

use std::collections::BTreeMap;

use dicom_ul::pdu::{PDataValue, PDataValueType, Pdu};

use crate::error::DicomError;
use crate::part10::{padded, trimmed_text};

/// Command Field (0000,0100) values.
pub mod command_field {
    /// C-STORE request.
    pub const C_STORE_RQ: u16 = 0x0001;
    /// C-STORE response.
    pub const C_STORE_RSP: u16 = 0x8001;
    /// C-GET request.
    pub const C_GET_RQ: u16 = 0x0010;
    /// C-GET response.
    pub const C_GET_RSP: u16 = 0x8010;
    /// C-FIND request.
    pub const C_FIND_RQ: u16 = 0x0020;
    /// C-FIND response.
    pub const C_FIND_RSP: u16 = 0x8020;
    /// C-MOVE request.
    pub const C_MOVE_RQ: u16 = 0x0021;
    /// C-MOVE response.
    pub const C_MOVE_RSP: u16 = 0x8021;
    /// C-ECHO request.
    pub const C_ECHO_RQ: u16 = 0x0030;
    /// C-ECHO response.
    pub const C_ECHO_RSP: u16 = 0x8030;
    /// N-EVENT-REPORT request.
    pub const N_EVENT_REPORT_RQ: u16 = 0x0100;
    /// N-EVENT-REPORT response.
    pub const N_EVENT_REPORT_RSP: u16 = 0x8100;
    /// N-SET request.
    pub const N_SET_RQ: u16 = 0x0120;
    /// N-SET response.
    pub const N_SET_RSP: u16 = 0x8120;
    /// N-ACTION request.
    pub const N_ACTION_RQ: u16 = 0x0130;
    /// N-ACTION response.
    pub const N_ACTION_RSP: u16 = 0x8130;
    /// N-CREATE request.
    pub const N_CREATE_RQ: u16 = 0x0140;
    /// N-CREATE response.
    pub const N_CREATE_RSP: u16 = 0x8140;
    /// C-CANCEL request, which has no response.
    pub const C_CANCEL_RQ: u16 = 0x0FFF;
}

/// Element numbers of command elements (group `0000`).
pub mod element {
    /// Affected SOP Class UID (0000,0002).
    pub const AFFECTED_SOP_CLASS_UID: u16 = 0x0002;
    /// Requested SOP Class UID (0000,0003).
    pub const REQUESTED_SOP_CLASS_UID: u16 = 0x0003;
    /// Command Field (0000,0100).
    pub const COMMAND_FIELD: u16 = 0x0100;
    /// Message ID (0000,0110).
    pub const MESSAGE_ID: u16 = 0x0110;
    /// Message ID Being Responded To (0000,0120).
    pub const MESSAGE_ID_BEING_RESPONDED_TO: u16 = 0x0120;
    /// Move Destination (0000,0600).
    pub const MOVE_DESTINATION: u16 = 0x0600;
    /// Priority (0000,0700).
    pub const PRIORITY: u16 = 0x0700;
    /// Command Data Set Type (0000,0800).
    pub const COMMAND_DATA_SET_TYPE: u16 = 0x0800;
    /// Status (0000,0900).
    pub const STATUS: u16 = 0x0900;
    /// Error Comment (0000,0902).
    pub const ERROR_COMMENT: u16 = 0x0902;
    /// Affected SOP Instance UID (0000,1000).
    pub const AFFECTED_SOP_INSTANCE_UID: u16 = 0x1000;
    /// Requested SOP Instance UID (0000,1001).
    pub const REQUESTED_SOP_INSTANCE_UID: u16 = 0x1001;
    /// Event Type ID (0000,1002).
    pub const EVENT_TYPE_ID: u16 = 0x1002;
    /// Action Type ID (0000,1008).
    pub const ACTION_TYPE_ID: u16 = 0x1008;
    /// Number of Remaining Sub-operations (0000,1020).
    pub const REMAINING_SUBOPERATIONS: u16 = 0x1020;
    /// Number of Completed Sub-operations (0000,1021).
    pub const COMPLETED_SUBOPERATIONS: u16 = 0x1021;
    /// Number of Failed Sub-operations (0000,1022).
    pub const FAILED_SUBOPERATIONS: u16 = 0x1022;
    /// Number of Warning Sub-operations (0000,1023).
    pub const WARNING_SUBOPERATIONS: u16 = 0x1023;
    /// Move Originator Application Entity Title (0000,1030).
    pub const MOVE_ORIGINATOR_AE_TITLE: u16 = 0x1030;
    /// Move Originator Message ID (0000,1031).
    pub const MOVE_ORIGINATOR_MESSAGE_ID: u16 = 0x1031;
}

/// DIMSE status codes used by OXIM.
pub mod status {
    /// Success.
    pub const SUCCESS: u16 = 0x0000;
    /// Warning: sub-operations complete, one or more failures or warnings
    /// (C-MOVE, C-GET).
    pub const SUBOPERATIONS_WITH_FAILURES: u16 = 0xB000;
    /// Failure: no such attribute (N-SET).
    pub const NO_SUCH_ATTRIBUTE: u16 = 0x0105;
    /// Failure: invalid attribute value.
    pub const INVALID_ATTRIBUTE_VALUE: u16 = 0x0106;
    /// Failure: processing failure.
    pub const PROCESSING_FAILURE: u16 = 0x0110;
    /// Failure: duplicate SOP instance (N-CREATE).
    pub const DUPLICATE_SOP_INSTANCE: u16 = 0x0111;
    /// Failure: no such SOP instance.
    pub const NO_SUCH_SOP_INSTANCE: u16 = 0x0112;
    /// Failure: no such event type.
    pub const NO_SUCH_EVENT_TYPE: u16 = 0x0113;
    /// Failure: no such action type (N-ACTION).
    pub const NO_SUCH_ACTION_TYPE: u16 = 0x0123;
    /// Failure: missing attribute (N-CREATE).
    pub const MISSING_ATTRIBUTE: u16 = 0x0120;
    /// Failure: missing attribute value.
    pub const MISSING_ATTRIBUTE_VALUE: u16 = 0x0121;
    /// Cancel: the operation was cancelled.
    pub const CANCEL: u16 = 0xFE00;
    /// Pending: more responses follow.
    pub const PENDING: u16 = 0xFF00;
    /// Pending: matches follow, some optional keys were not supported
    /// (C-FIND).
    pub const PENDING_WITH_WARNINGS: u16 = 0xFF01;
    /// Refused: move destination unknown (C-MOVE).
    pub const MOVE_DESTINATION_UNKNOWN: u16 = 0xA801;
    /// Failure: identifier does not match SOP class (C-FIND, C-MOVE,
    /// C-GET).
    pub const IDENTIFIER_DOES_NOT_MATCH_SOP_CLASS: u16 = 0xA900;
    /// Failure: unable to process (C-FIND, C-MOVE, C-GET).
    pub const UNABLE_TO_PROCESS: u16 = 0xC000;
    /// Failure: SOP class not supported.
    pub const SOP_CLASS_NOT_SUPPORTED: u16 = 0x0122;
    /// Failure: unrecognized operation.
    pub const UNRECOGNIZED_OPERATION: u16 = 0x0211;
    /// Failure: resource limitation.
    pub const RESOURCE_LIMITATION: u16 = 0x0213;
    /// Refused: out of resources (C-STORE).
    pub const OUT_OF_RESOURCES: u16 = 0xA700;
    /// Error: data set does not match SOP class (C-STORE).
    pub const DATA_SET_DOES_NOT_MATCH_SOP_CLASS: u16 = 0xA900;
    /// Error: cannot understand (C-STORE).
    pub const CANNOT_UNDERSTAND: u16 = 0xC000;
    /// Warning: coercion of data elements (C-STORE).
    pub const COERCION_OF_DATA_ELEMENTS: u16 = 0xB000;
}

/// The Command Data Set Type (0000,0800) value that means "no data set".
pub const NO_DATA_SET: u16 = 0x0101;

/// What a status code says about a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// The operation succeeded.
    Success,
    /// The operation succeeded with a warning, for example after coercing
    /// data elements.
    Warning,
    /// The operation failed.
    Failure,
    /// The operation was cancelled.
    Cancel,
    /// More responses follow.
    Pending,
}

impl StatusKind {
    /// Classifies a status code (PS3.7 annex C).
    pub fn of(status: u16) -> Self {
        match status {
            0x0000 => Self::Success,
            0x0001 | 0x0107 | 0x0116 | 0xB000..=0xBFFF => Self::Warning,
            0xFE00 => Self::Cancel,
            0xFF00 | 0xFF01 => Self::Pending,
            _ => Self::Failure,
        }
    }
}

/// A DIMSE command set: the elements of group `0000`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Command {
    elements: BTreeMap<u16, Vec<u8>>,
}

impl Command {
    /// An empty command.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an unsigned short (US) element.
    pub fn with_u16(mut self, element: u16, value: u16) -> Self {
        self.elements.insert(element, value.to_le_bytes().to_vec());
        self
    }

    /// Sets a UID (UI) element.
    pub fn with_uid(mut self, element: u16, uid: &str) -> Self {
        self.elements.insert(element, padded(uid, 64, 0));
        self
    }

    /// Sets a text element (AE, LO), cut to 64 bytes.
    pub fn with_text(mut self, element: u16, text: &str) -> Self {
        self.elements.insert(element, padded(text, 64, b' '));
        self
    }

    /// An unsigned short element.
    pub fn u16(&self, element: u16) -> Option<u16> {
        match self.elements.get(&element)?.as_slice() {
            [low, high, ..] => Some(u16::from_le_bytes([*low, *high])),
            _ => None,
        }
    }

    /// A text or UID element without padding.
    pub fn text(&self, element: u16) -> Option<String> {
        self.elements.get(&element).map(|value| trimmed_text(value))
    }

    /// Command Field (0000,0100).
    pub fn command_field(&self) -> Option<u16> {
        self.u16(element::COMMAND_FIELD)
    }

    /// Message ID (0000,0110).
    pub fn message_id(&self) -> Option<u16> {
        self.u16(element::MESSAGE_ID)
    }

    /// Status (0000,0900).
    pub fn status(&self) -> Option<u16> {
        self.u16(element::STATUS)
    }

    /// Whether a data set follows the command.
    pub fn has_data_set(&self) -> bool {
        self.u16(element::COMMAND_DATA_SET_TYPE)
            .is_some_and(|kind| kind != NO_DATA_SET)
    }

    /// A C-ECHO request.
    pub fn c_echo_rq(message_id: u16) -> Self {
        Self::new()
            .with_uid(element::AFFECTED_SOP_CLASS_UID, crate::uids::VERIFICATION)
            .with_u16(element::COMMAND_FIELD, command_field::C_ECHO_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::COMMAND_DATA_SET_TYPE, NO_DATA_SET)
    }

    /// A C-FIND request with medium priority; the identifier follows.
    pub fn c_find_rq(message_id: u16, sop_class_uid: &str) -> Self {
        Self::new()
            .with_uid(element::AFFECTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::C_FIND_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::PRIORITY, 0)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
    }

    /// A C-MOVE request with medium priority to `destination`; the
    /// identifier follows.
    pub fn c_move_rq(message_id: u16, sop_class_uid: &str, destination: &str) -> Self {
        Self::new()
            .with_uid(element::AFFECTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::C_MOVE_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::PRIORITY, 0)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
            .with_text(element::MOVE_DESTINATION, destination)
    }

    /// A C-GET request with medium priority; the identifier follows.
    pub fn c_get_rq(message_id: u16, sop_class_uid: &str) -> Self {
        Self::new()
            .with_uid(element::AFFECTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::C_GET_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::PRIORITY, 0)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
    }

    /// A C-CANCEL request for the operation `message_id`.
    pub fn c_cancel_rq(message_id: u16) -> Self {
        Self::new()
            .with_u16(element::COMMAND_FIELD, command_field::C_CANCEL_RQ)
            .with_u16(element::MESSAGE_ID_BEING_RESPONDED_TO, message_id)
            .with_u16(element::COMMAND_DATA_SET_TYPE, NO_DATA_SET)
    }

    /// An N-CREATE request; the attribute list follows.
    pub fn n_create_rq(message_id: u16, sop_class_uid: &str, sop_instance_uid: &str) -> Self {
        Self::new()
            .with_uid(element::AFFECTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::N_CREATE_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
            .with_uid(element::AFFECTED_SOP_INSTANCE_UID, sop_instance_uid)
    }

    /// An N-SET request; the modification list follows.
    pub fn n_set_rq(message_id: u16, sop_class_uid: &str, sop_instance_uid: &str) -> Self {
        Self::new()
            .with_uid(element::REQUESTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::N_SET_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
            .with_uid(element::REQUESTED_SOP_INSTANCE_UID, sop_instance_uid)
    }

    /// An N-ACTION request; the action information follows.
    pub fn n_action_rq(
        message_id: u16,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        action_type: u16,
    ) -> Self {
        Self::new()
            .with_uid(element::REQUESTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::N_ACTION_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
            .with_uid(element::REQUESTED_SOP_INSTANCE_UID, sop_instance_uid)
            .with_u16(element::ACTION_TYPE_ID, action_type)
    }

    /// An N-EVENT-REPORT request; the event information follows.
    pub fn n_event_report_rq(
        message_id: u16,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        event_type: u16,
    ) -> Self {
        Self::new()
            .with_uid(element::AFFECTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::N_EVENT_REPORT_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
            .with_uid(element::AFFECTED_SOP_INSTANCE_UID, sop_instance_uid)
            .with_u16(element::EVENT_TYPE_ID, event_type)
    }

    /// Marks the command as followed by a data set.
    pub fn with_data_set(self) -> Self {
        self.with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
    }

    /// Sets the sub-operation counters of a C-MOVE or C-GET response.
    pub fn with_suboperations(
        self,
        remaining: u16,
        completed: u16,
        failed: u16,
        warning: u16,
    ) -> Self {
        self.with_u16(element::REMAINING_SUBOPERATIONS, remaining)
            .with_u16(element::COMPLETED_SUBOPERATIONS, completed)
            .with_u16(element::FAILED_SUBOPERATIONS, failed)
            .with_u16(element::WARNING_SUBOPERATIONS, warning)
    }

    /// The SOP class UID a request names: Affected SOP Class UID, or
    /// Requested SOP Class UID for N-SET, N-GET and N-ACTION.
    pub fn sop_class_uid(&self) -> Option<String> {
        self.text(element::AFFECTED_SOP_CLASS_UID)
            .filter(|uid| !uid.is_empty())
            .or_else(|| self.text(element::REQUESTED_SOP_CLASS_UID))
            .filter(|uid| !uid.is_empty())
    }

    /// The SOP instance UID a request names: Affected SOP Instance UID, or
    /// Requested SOP Instance UID.
    pub fn sop_instance_uid(&self) -> Option<String> {
        self.text(element::AFFECTED_SOP_INSTANCE_UID)
            .filter(|uid| !uid.is_empty())
            .or_else(|| self.text(element::REQUESTED_SOP_INSTANCE_UID))
            .filter(|uid| !uid.is_empty())
    }

    /// A C-STORE request with medium priority; the data set follows.
    pub fn c_store_rq(message_id: u16, sop_class_uid: &str, sop_instance_uid: &str) -> Self {
        Self::new()
            .with_uid(element::AFFECTED_SOP_CLASS_UID, sop_class_uid)
            .with_u16(element::COMMAND_FIELD, command_field::C_STORE_RQ)
            .with_u16(element::MESSAGE_ID, message_id)
            .with_u16(element::PRIORITY, 0)
            .with_u16(element::COMMAND_DATA_SET_TYPE, 0x0000)
            .with_uid(element::AFFECTED_SOP_INSTANCE_UID, sop_instance_uid)
    }

    /// The response to `request` with `status` and an optional error
    /// comment.
    pub fn response_to(request: &Command, status: u16, comment: Option<&str>) -> Self {
        let mut response = Self::new()
            .with_u16(
                element::COMMAND_FIELD,
                request.command_field().unwrap_or_default() | 0x8000,
            )
            .with_u16(
                element::MESSAGE_ID_BEING_RESPONDED_TO,
                request.message_id().unwrap_or_default(),
            )
            .with_u16(element::COMMAND_DATA_SET_TYPE, NO_DATA_SET)
            .with_u16(element::STATUS, status);
        // N-SET, N-GET and N-ACTION requests name the instance with the
        // Requested UIDs; their responses carry them as Affected UIDs.
        for (affected, requested) in [
            (
                element::AFFECTED_SOP_CLASS_UID,
                element::REQUESTED_SOP_CLASS_UID,
            ),
            (
                element::AFFECTED_SOP_INSTANCE_UID,
                element::REQUESTED_SOP_INSTANCE_UID,
            ),
        ] {
            if let Some(value) = request
                .elements
                .get(&affected)
                .or_else(|| request.elements.get(&requested))
            {
                response.elements.insert(affected, value.clone());
            }
        }
        for copied in [element::ACTION_TYPE_ID, element::EVENT_TYPE_ID] {
            if let Some(value) = request.elements.get(&copied) {
                response.elements.insert(copied, value.clone());
            }
        }
        if let Some(comment) = comment {
            response = response.with_text(element::ERROR_COMMENT, comment);
        }
        response
    }

    /// Encodes the command with its group length.
    pub fn encode(&self) -> Vec<u8> {
        let body_length: usize = self.elements.values().map(|value| 8 + value.len()).sum();
        let mut out = Vec::with_capacity(12 + body_length);
        let mut put = |element: u16, value: &[u8]| {
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&element.to_le_bytes());
            out.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_le_bytes());
            out.extend_from_slice(value);
        };
        put(
            0x0000,
            &u32::try_from(body_length).unwrap_or(u32::MAX).to_le_bytes(),
        );
        for (element, value) in &self.elements {
            put(*element, value);
        }
        out
    }

    /// Decodes a command set.
    pub fn decode(bytes: &[u8]) -> Result<Self, DicomError> {
        let mut elements = BTreeMap::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let [g0, g1, e0, e1, l0, l1, l2, l3, ..] = *rest else {
                return Err(DicomError::invalid("truncated command element"));
            };
            if u16::from_le_bytes([g0, g1]) != 0x0000 {
                return Err(DicomError::invalid(
                    "a command set may only contain group 0000",
                ));
            }
            let element = u16::from_le_bytes([e0, e1]);
            let end = usize::try_from(u32::from_le_bytes([l0, l1, l2, l3]))
                .ok()
                .and_then(|length| length.checked_add(8))
                .filter(|end| *end <= rest.len())
                .ok_or_else(|| DicomError::invalid("truncated command element"))?;
            if element != 0x0000 {
                elements.insert(element, rest[8..end].to_vec());
            }
            rest = &rest[end..];
        }
        let command = Self { elements };
        if command.command_field().is_none() {
            return Err(DicomError::invalid("the command has no Command Field"));
        }
        Ok(command)
    }
}

/// The largest command set accepted.
const MAX_COMMAND: usize = 64 * 1024;

/// A command with its data set, reassembled from P-DATA fragments.
#[derive(Debug)]
pub(crate) struct Message {
    pub(crate) context_id: u8,
    pub(crate) command: Command,
    pub(crate) data: Vec<u8>,
    /// The data set exceeded the limit and was discarded.
    pub(crate) oversized: bool,
}

/// Reassembles commands and data sets from presentation data values.
#[derive(Debug)]
pub(crate) struct Assembler {
    limit: usize,
    command: Vec<u8>,
    command_context: Option<u8>,
    waiting: Option<(u8, Command)>,
    data: Vec<u8>,
    oversized: bool,
}

impl Assembler {
    /// Data sets larger than `limit` bytes are discarded while they arrive.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            command: Vec::new(),
            command_context: None,
            waiting: None,
            data: Vec::new(),
            oversized: false,
        }
    }

    /// Adds one fragment; returns a message once it is complete.
    pub(crate) fn push(&mut self, value: PDataValue) -> Result<Option<Message>, DicomError> {
        let context_id = value.presentation_context_id;
        match value.value_type {
            PDataValueType::Command => {
                if self.waiting.is_some() {
                    return Err(DicomError::invalid(
                        "a command arrived while a data set was expected",
                    ));
                }
                if self.command_context.is_some_and(|id| id != context_id) {
                    return Err(DicomError::invalid(
                        "command fragments use different presentation contexts",
                    ));
                }
                if self.command.len() + value.data.len() > MAX_COMMAND {
                    return Err(DicomError::invalid("the command set is too large"));
                }
                self.command_context = Some(context_id);
                self.command.extend_from_slice(&value.data);
                if !value.is_last {
                    return Ok(None);
                }
                self.command_context = None;
                let command = Command::decode(&std::mem::take(&mut self.command))?;
                if command.has_data_set() {
                    self.waiting = Some((context_id, command));
                    return Ok(None);
                }
                Ok(Some(Message {
                    context_id,
                    command,
                    data: Vec::new(),
                    oversized: false,
                }))
            }
            PDataValueType::Data => {
                match &self.waiting {
                    None => {
                        return Err(DicomError::invalid("a data set arrived without a command"));
                    }
                    Some((expected, _)) if *expected != context_id => {
                        return Err(DicomError::invalid(
                            "the data set uses another presentation context than its command",
                        ));
                    }
                    Some(_) => {}
                }
                if !self.oversized {
                    if self.data.len().saturating_add(value.data.len()) > self.limit {
                        self.oversized = true;
                        self.data = Vec::new();
                    } else if self.data.is_empty() {
                        self.data = value.data;
                    } else {
                        self.data.extend_from_slice(&value.data);
                    }
                }
                if !value.is_last {
                    return Ok(None);
                }
                let Some((context_id, command)) = self.waiting.take() else {
                    return Err(DicomError::invalid("a data set arrived without a command"));
                };
                Ok(Some(Message {
                    context_id,
                    command,
                    data: std::mem::take(&mut self.data),
                    oversized: std::mem::replace(&mut self.oversized, false),
                }))
            }
        }
    }
}

/// The largest fragment OXIM sends in one PDU, to bound copies.
const MAX_FRAGMENT: usize = 1024 * 1024;

/// Splits `data` into P-DATA-TF PDUs no larger than the peer's maximum PDU
/// length.
pub(crate) fn pdus(
    context_id: u8,
    value_type: PDataValueType,
    data: &[u8],
    max_pdu_length: u32,
) -> impl Iterator<Item = Pdu> + '_ {
    // Each value carries a 6-byte item header: length, context, control.
    // A maximum of 0 means unlimited.
    let size = if max_pdu_length == 0 {
        MAX_FRAGMENT
    } else {
        usize::try_from(max_pdu_length.saturating_sub(6))
            .unwrap_or(MAX_FRAGMENT)
            .clamp(1, MAX_FRAGMENT)
    };
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![data]
    } else {
        data.chunks(size).collect()
    };
    let count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(move |(index, chunk)| Pdu::PData {
            data: vec![PDataValue {
                presentation_context_id: context_id,
                value_type: value_type.clone(),
                is_last: index + 1 == count,
                data: chunk.to_vec(),
            }],
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(value_type: PDataValueType, is_last: bool, data: &[u8]) -> PDataValue {
        PDataValue {
            presentation_context_id: 1,
            value_type,
            is_last,
            data: data.to_vec(),
        }
    }

    #[test]
    fn encodes_and_decodes_commands() {
        let request = Command::c_store_rq(7, "1.2.840.10008.5.1.4.1.1.7", "1.2.3");
        let bytes = request.encode();
        // Group length element first, value = remaining bytes.
        assert_eq!(&bytes[..8], &[0, 0, 0, 0, 4, 0, 0, 0]);
        let length = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        assert_eq!(length, bytes.len() - 12);
        let decoded = Command::decode(&bytes).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(decoded.command_field(), Some(command_field::C_STORE_RQ));
        assert_eq!(
            decoded.text(element::AFFECTED_SOP_INSTANCE_UID).as_deref(),
            Some("1.2.3")
        );
        assert!(decoded.has_data_set());

        let response = Command::response_to(&decoded, status::OUT_OF_RESOURCES, Some("full"));
        assert_eq!(response.command_field(), Some(command_field::C_STORE_RSP));
        assert_eq!(
            response.u16(element::MESSAGE_ID_BEING_RESPONDED_TO),
            Some(7)
        );
        assert_eq!(response.status(), Some(0xA700));
        assert_eq!(
            response.text(element::ERROR_COMMENT).as_deref(),
            Some("full")
        );
        assert!(!response.has_data_set());

        assert!(Command::decode(b"\x08\x00\x00\x00\x00\x00\x00\x00").is_err());
        assert!(Command::decode(b"\x00\x00\x00\x01\xff\x00\x00\x00").is_err());
        assert!(Command::decode(b"").is_err());
    }

    #[test]
    fn classifies_statuses() {
        assert_eq!(StatusKind::of(0x0000), StatusKind::Success);
        assert_eq!(StatusKind::of(0xB007), StatusKind::Warning);
        assert_eq!(StatusKind::of(0xA700), StatusKind::Failure);
        assert_eq!(StatusKind::of(0xC001), StatusKind::Failure);
        assert_eq!(StatusKind::of(0xFF00), StatusKind::Pending);
    }

    #[test]
    fn reassembles_messages() {
        let command = Command::c_store_rq(1, "1.2", "1.2.3").encode();
        let mut assembler = Assembler::new(8);
        let (first, second) = command.split_at(10);
        assert!(
            assembler
                .push(value(PDataValueType::Command, false, first))
                .unwrap()
                .is_none()
        );
        assert!(
            assembler
                .push(value(PDataValueType::Command, true, second))
                .unwrap()
                .is_none()
        );
        assert!(
            assembler
                .push(value(PDataValueType::Data, false, b"abcd"))
                .unwrap()
                .is_none()
        );
        let message = assembler
            .push(value(PDataValueType::Data, true, b"efgh"))
            .unwrap()
            .unwrap();
        assert_eq!(message.data, b"abcdefgh");
        assert!(!message.oversized);

        assembler
            .push(value(PDataValueType::Command, true, &command))
            .unwrap();
        assembler
            .push(value(PDataValueType::Data, false, b"abcdefgh"))
            .unwrap();
        let message = assembler
            .push(value(PDataValueType::Data, true, b"i"))
            .unwrap()
            .unwrap();
        assert!(message.oversized);
        assert!(message.data.is_empty());

        let echo = Command::c_echo_rq(2).encode();
        let message = assembler
            .push(value(PDataValueType::Command, true, &echo))
            .unwrap()
            .unwrap();
        assert_eq!(
            message.command.command_field(),
            Some(command_field::C_ECHO_RQ)
        );

        assert!(
            assembler
                .push(value(PDataValueType::Data, true, b"x"))
                .is_err()
        );
    }

    #[test]
    fn splits_data_into_pdus() {
        let data = vec![7u8; 25];
        let split: Vec<Pdu> = pdus(3, PDataValueType::Data, &data, 16).collect();
        assert_eq!(split.len(), 3);
        let Pdu::PData { data: last } = &split[2] else {
            unreachable!()
        };
        assert!(last[0].is_last);
        assert_eq!(last[0].data.len(), 5);
        assert_eq!(pdus(1, PDataValueType::Data, &[], 16).count(), 1);
    }
}
