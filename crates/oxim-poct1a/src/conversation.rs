//! The host side of a POCT1-A conversation.
//!
//! [`HostConversation`] plays the observation reviewer (data manager) role
//! for one device connection. It is a sans-IO state machine: feed it parsed
//! device messages with [`HostConversation::handle_message`], call
//! [`HostConversation::handle_timeout`] when the deadline from
//! [`HostConversation::poll_timeout`] passes, and drain
//! [`HostConversation::poll_output`] after each call. Outputs are either
//! messages to send ([`Output::Send`]) or information for the application.
//!
//! A typical conversation:
//!
//! 1. The device sends `HEL` (hello); the host acknowledges it.
//! 2. The device sends `DST` (status); the host acknowledges it and, when
//!    the device reports new observations, sends `REQ` asking for them.
//! 3. The device sends `OBS` messages, each acknowledged, then `EOT` to end
//!    the topic.
//! 4. Either side sends `END` to finish.
//!
//! Observation and event messages are delivered to the application before
//! they are acknowledged. With [`DataAcknowledgment::Manual`] (the default)
//! the host sends no acknowledgment until the application calls
//! [`HostConversation::acknowledge`], so a result is only accepted after it
//! has been stored durably.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::builder::{self, Header, REQUEST_OBSERVATIONS, VERSION_ID};
use crate::content::{AckType, DeviceInfo, DeviceStatus};
use crate::element::is_xml_char;
use crate::error::{BuildError, ConversationError};
use crate::message::{Message, MessageKind};

/// The current time, supplied by the caller: a monotonic instant for timers
/// and the wall-clock time written to `HDR.creation_dttm` (ISO 8601 with
/// offset, for example `2026-09-29T12:00:00+03:00`).
#[derive(Debug, Clone, Copy)]
pub struct Now<'a> {
    /// Monotonic time used for deadlines.
    pub instant: Instant,
    /// Wall-clock time for message headers.
    pub datetime: &'a str,
}

/// When observation (`OBS`) and event (`EVS`) messages are acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DataAcknowledgment {
    /// The application acknowledges each delivery with
    /// [`HostConversation::acknowledge`], after storing it.
    #[default]
    Manual,
    /// The conversation acknowledges each delivery right after emitting it.
    Automatic,
}

/// Configuration of a [`HostConversation`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostConfig {
    /// `HDR.version_id` of host messages.
    pub version_id: String,
    /// The first `HDR.control_id` used by the host.
    pub first_control_id: u64,
    /// How long to wait for the device's hello after the connection opens.
    pub hello_timeout: Duration,
    /// How long to wait for the device to acknowledge a host message.
    pub response_timeout: Duration,
    /// End the conversation when the device sends nothing for this long.
    pub idle_timeout: Option<Duration>,
    /// Send a keep-alive (`KPA`) when the host has sent nothing for this
    /// long while the conversation is idle. Off by default because not every
    /// device supports it.
    pub keep_alive_interval: Option<Duration>,
    /// Request observations when a status message reports new ones.
    pub request_observations_on_status: bool,
    /// `REQ.request_cd` used to request observations.
    pub observations_request_code: String,
    /// When data messages are acknowledged.
    pub data_acknowledgment: DataAcknowledgment,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            version_id: VERSION_ID.to_owned(),
            first_control_id: 1,
            hello_timeout: Duration::from_secs(60),
            response_timeout: Duration::from_secs(30),
            idle_timeout: Some(Duration::from_secs(300)),
            keep_alive_interval: None,
            request_observations_on_status: true,
            observations_request_code: REQUEST_OBSERVATIONS.to_owned(),
            data_acknowledgment: DataAcknowledgment::Manual,
        }
    }
}

/// The phase of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationState {
    /// Waiting for the device's hello (`HEL`).
    AwaitingHello,
    /// Hello acknowledged; waiting for the device status (`DST`).
    AwaitingStatus,
    /// Ready for observations, requests and directives.
    Ready,
    /// Inside a topic started by a host request; ends with `EOT`.
    InTopic,
    /// The conversation has ended.
    Closed,
}

/// How the application answers a delivered data message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeliveryOutcome {
    /// Stored successfully: acknowledge with `AA`.
    Accepted,
    /// Could not be processed: acknowledge with `AE` and a note.
    Error(String),
    /// Refused: acknowledge with `AR` and a note.
    Rejected(String),
}

/// Why a conversation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CloseReason {
    /// The host sent `END` and the device acknowledged it.
    HostTerminated,
    /// The device sent `END`.
    DeviceTerminated,
    /// No hello arrived within [`HostConfig::hello_timeout`].
    HelloTimeout,
    /// The device did not acknowledge a host message in time.
    ResponseTimeout,
    /// The device sent nothing for [`HostConfig::idle_timeout`].
    IdleTimeout,
}

/// Something the conversation wants the caller to do or know.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Output {
    /// Send this message to the device.
    Send(Message),
    /// The device introduced itself.
    Hello(DeviceInfo),
    /// The device reported its status.
    Status(DeviceStatus),
    /// Observations to store. With manual acknowledgment, call
    /// [`HostConversation::acknowledge`] with the message's control ID.
    Observations(Message),
    /// Device events to store; acknowledged like observations.
    Events(Message),
    /// The device ended a topic, for example `OBS`.
    TopicEnded(String),
    /// The device accepted a host message.
    Accepted {
        /// Control ID of the host message.
        control_id: String,
    },
    /// The device answered a host message with `AE` or `AR`, or with an
    /// acknowledgment without a valid type.
    Rejected {
        /// Control ID of the host message.
        control_id: String,
        /// The acknowledgment type (`AE` when missing or invalid).
        ack_type: AckType,
        /// `ACK.note_txt`, if any.
        note: Option<String>,
    },
    /// The conversation ended; close the connection.
    Closed(CloseReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    AwaitingHello,
    AwaitingStatus,
    Ready,
    InTopic(String),
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Purpose {
    Request { topic: String },
    Directive,
    Terminate,
    KeepAlive,
}

#[derive(Debug, Clone)]
struct Pending {
    control_id: String,
    purpose: Purpose,
    deadline: Instant,
}

/// The host side of one device conversation. See the [module
/// documentation](self).
#[derive(Debug, Clone)]
pub struct HostConversation {
    config: HostConfig,
    phase: Phase,
    device: Option<DeviceInfo>,
    next_control_id: u64,
    pending: Option<Pending>,
    awaiting_application: Vec<String>,
    outputs: VecDeque<Output>,
    hello_deadline: Instant,
    last_received: Instant,
    last_sent: Instant,
}

impl HostConversation {
    /// Starts a conversation for a connection opened at `now`.
    pub fn new(config: HostConfig, now: Instant) -> Result<Self, ConversationError> {
        check_text("HDR.version_id", &config.version_id)?;
        check_text("REQ.request_cd", &config.observations_request_code)?;
        Ok(Self {
            hello_deadline: now + config.hello_timeout,
            next_control_id: config.first_control_id,
            config,
            phase: Phase::AwaitingHello,
            device: None,
            pending: None,
            awaiting_application: Vec::new(),
            outputs: VecDeque::new(),
            last_received: now,
            last_sent: now,
        })
    }

    /// The current phase.
    pub fn state(&self) -> ConversationState {
        match self.phase {
            Phase::AwaitingHello => ConversationState::AwaitingHello,
            Phase::AwaitingStatus => ConversationState::AwaitingStatus,
            Phase::Ready => ConversationState::Ready,
            Phase::InTopic(_) => ConversationState::InTopic,
            Phase::Closed => ConversationState::Closed,
        }
    }

    /// The topic of the current host request, while in
    /// [`ConversationState::InTopic`].
    pub fn topic(&self) -> Option<&str> {
        match &self.phase {
            Phase::InTopic(topic) => Some(topic),
            _ => None,
        }
    }

    /// The device identification from its hello, once received.
    pub fn device(&self) -> Option<&DeviceInfo> {
        self.device.as_ref()
    }

    /// Whether the conversation has ended.
    pub fn is_closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// Control IDs of delivered data messages the application has not yet
    /// acknowledged.
    pub fn awaiting_acknowledgment(&self) -> &[String] {
        &self.awaiting_application
    }

    /// The next output, if any.
    pub fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }

    /// The earliest instant at which [`HostConversation::handle_timeout`]
    /// must be called, or `None` when no timer is running.
    pub fn poll_timeout(&self) -> Option<Instant> {
        if self.phase == Phase::Closed {
            return None;
        }
        let hello = (self.phase == Phase::AwaitingHello).then_some(self.hello_deadline);
        let pending = self.pending.as_ref().map(|p| p.deadline);
        let quiet = self.awaiting_application.is_empty();
        let idle = self
            .config
            .idle_timeout
            .filter(|_| quiet)
            .map(|timeout| self.last_received + timeout);
        let keep_alive = self
            .config
            .keep_alive_interval
            .filter(|_| quiet && self.pending.is_none() && self.phase == Phase::Ready)
            .map(|interval| self.last_sent + interval);
        [hello, pending, idle, keep_alive]
            .into_iter()
            .flatten()
            .min()
    }

    /// Processes expired timers.
    pub fn handle_timeout(&mut self, now: Now<'_>) -> Result<(), ConversationError> {
        if self.phase == Phase::Closed {
            return Ok(());
        }
        check_text("HDR.creation_dttm", now.datetime)?;
        let at = now.instant;
        if self.phase == Phase::AwaitingHello && at >= self.hello_deadline {
            self.close(CloseReason::HelloTimeout);
            return Ok(());
        }
        if let Some(pending) = &self.pending
            && at >= pending.deadline
        {
            if pending.purpose != Purpose::Terminate {
                self.send_terminate(now)?;
            }
            self.close(CloseReason::ResponseTimeout);
            return Ok(());
        }
        if let Some(timeout) = self.config.idle_timeout
            && self.awaiting_application.is_empty()
            && at >= self.last_received + timeout
        {
            self.send_terminate(now)?;
            self.close(CloseReason::IdleTimeout);
            return Ok(());
        }
        if let Some(interval) = self.config.keep_alive_interval
            && self.phase == Phase::Ready
            && self.pending.is_none()
            && self.awaiting_application.is_empty()
            && at >= self.last_sent + interval
        {
            let control_id = self.next_id();
            let message = builder::keep_alive(&self.header(&control_id, now))?;
            self.expect_ack(control_id, Purpose::KeepAlive, at);
            self.send(message, at);
        }
        Ok(())
    }

    /// Processes a message received from the device.
    pub fn handle_message(
        &mut self,
        message: Message,
        now: Now<'_>,
    ) -> Result<(), ConversationError> {
        if self.phase == Phase::Closed {
            return Ok(());
        }
        check_text("HDR.creation_dttm", now.datetime)?;
        self.last_received = now.instant;
        let kind = message.kind();
        if kind == MessageKind::Acknowledgment {
            self.on_ack(&message);
            return Ok(());
        }
        let Some(control_id) = message.control_id().map(str::to_owned) else {
            return self.reply("", AckType::Reject, Some("missing HDR.control_id"), now);
        };
        if self.phase == Phase::AwaitingHello && kind != MessageKind::Hello {
            return self.reply(&control_id, AckType::Reject, Some("expected HEL.R01"), now);
        }
        match kind {
            MessageKind::Hello => {
                let info = message.device_info().unwrap_or_default();
                self.device = Some(info.clone());
                self.outputs.push_back(Output::Hello(info));
                self.phase = Phase::AwaitingStatus;
                self.reply(&control_id, AckType::Accept, None, now)
            }
            MessageKind::DeviceStatus => {
                let status = message.device_status().unwrap_or_default();
                let new_observations = status.new_observations.unwrap_or(0);
                self.outputs.push_back(Output::Status(status));
                if self.phase == Phase::AwaitingStatus {
                    self.phase = Phase::Ready;
                }
                self.reply(&control_id, AckType::Accept, None, now)?;
                if self.config.request_observations_on_status
                    && new_observations > 0
                    && self.phase == Phase::Ready
                    && self.pending.is_none()
                {
                    self.request_observations(now)?;
                }
                Ok(())
            }
            MessageKind::Observation | MessageKind::Event => {
                if self.awaiting_application.contains(&control_id) {
                    // A retransmission of a delivery that is still being
                    // stored: the application will acknowledge it once.
                    return Ok(());
                }
                self.outputs.push_back(if kind == MessageKind::Observation {
                    Output::Observations(message)
                } else {
                    Output::Events(message)
                });
                match self.config.data_acknowledgment {
                    DataAcknowledgment::Automatic => {
                        self.reply(&control_id, AckType::Accept, None, now)
                    }
                    DataAcknowledgment::Manual => {
                        self.awaiting_application.push(control_id);
                        Ok(())
                    }
                }
            }
            MessageKind::EndOfTopic => {
                let topic = message
                    .end_of_topic()
                    .map(str::to_owned)
                    .or_else(|| self.topic().map(str::to_owned))
                    .unwrap_or_default();
                self.outputs.push_back(Output::TopicEnded(topic));
                if matches!(self.phase, Phase::InTopic(_)) {
                    self.phase = Phase::Ready;
                }
                self.reply(&control_id, AckType::Accept, None, now)
            }
            MessageKind::Terminate => {
                self.reply(&control_id, AckType::Accept, None, now)?;
                self.close(CloseReason::DeviceTerminated);
                Ok(())
            }
            MessageKind::KeepAlive => self.reply(&control_id, AckType::Accept, None, now),
            MessageKind::Request | MessageKind::Directive | MessageKind::OperatorList => self
                .reply(
                    &control_id,
                    AckType::Reject,
                    Some("message type is not accepted from a device"),
                    now,
                ),
            _ => self.reply(
                &control_id,
                AckType::Reject,
                Some("unsupported message type"),
                now,
            ),
        }
    }

    /// Acknowledges a delivered observation or event message after the
    /// application has processed it.
    pub fn acknowledge(
        &mut self,
        control_id: &str,
        outcome: DeliveryOutcome,
        now: Now<'_>,
    ) -> Result<(), ConversationError> {
        if self.phase == Phase::Closed {
            return Err(ConversationError::Closed);
        }
        check_text("HDR.creation_dttm", now.datetime)?;
        let (ack_type, note) = match &outcome {
            DeliveryOutcome::Accepted => (AckType::Accept, None),
            DeliveryOutcome::Error(note) => (AckType::Error, Some(note.as_str())),
            DeliveryOutcome::Rejected(note) => (AckType::Reject, Some(note.as_str())),
        };
        if let Some(note) = note {
            check_text("ACK.note_txt", note)?;
        }
        let position = self
            .awaiting_application
            .iter()
            .position(|id| id == control_id)
            .ok_or_else(|| ConversationError::UnknownDelivery(control_id.to_owned()))?;
        self.awaiting_application.remove(position);
        self.reply(control_id, ack_type, note, now)
    }

    /// Asks the device for its observations. The device acknowledges the
    /// request, sends `OBS` messages and ends the topic with `EOT`.
    pub fn request_observations(&mut self, now: Now<'_>) -> Result<(), ConversationError> {
        self.ensure_idle()?;
        check_text("HDR.creation_dttm", now.datetime)?;
        let control_id = self.next_id();
        let code = self.config.observations_request_code.clone();
        let message = builder::request(&self.header(&control_id, now), &code)?;
        self.expect_ack(
            control_id,
            Purpose::Request {
                topic: "OBS".to_owned(),
            },
            now.instant,
        );
        self.send(message, now.instant);
        Ok(())
    }

    /// Sends a directive (`DTV`) such as
    /// [`builder::directive::START_CONTINUOUS`].
    pub fn send_directive(
        &mut self,
        command_cd: &str,
        parameters: &[(&str, &str)],
        now: Now<'_>,
    ) -> Result<(), ConversationError> {
        self.ensure_idle()?;
        check_text("HDR.creation_dttm", now.datetime)?;
        let control_id = self.next_id();
        let message = builder::directive(&self.header(&control_id, now), command_cd, parameters)?;
        self.expect_ack(control_id, Purpose::Directive, now.instant);
        self.send(message, now.instant);
        Ok(())
    }

    /// Ends the conversation with `END`. The conversation closes when the
    /// device acknowledges it or the response timeout passes.
    pub fn terminate(&mut self, now: Now<'_>) -> Result<(), ConversationError> {
        if self.phase == Phase::Closed {
            return Err(ConversationError::Closed);
        }
        check_text("HDR.creation_dttm", now.datetime)?;
        let control_id = self.send_terminate(now)?;
        self.expect_ack(control_id, Purpose::Terminate, now.instant);
        Ok(())
    }

    fn on_ack(&mut self, message: &Message) {
        let Some(info) = message.ack_info() else {
            return;
        };
        let matches = self
            .pending
            .as_ref()
            .is_some_and(|p| info.acked_control_id.as_deref() == Some(p.control_id.as_str()));
        if !matches {
            // A late or unrelated acknowledgment.
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        match info.ack_type {
            Some(AckType::Accept) => {
                self.outputs.push_back(Output::Accepted {
                    control_id: pending.control_id,
                });
                match pending.purpose {
                    Purpose::Request { topic } => self.phase = Phase::InTopic(topic),
                    Purpose::Terminate => self.close(CloseReason::HostTerminated),
                    Purpose::Directive | Purpose::KeepAlive => {}
                }
            }
            other => {
                self.outputs.push_back(Output::Rejected {
                    control_id: pending.control_id,
                    ack_type: other.unwrap_or(AckType::Error),
                    note: info.note,
                });
                if pending.purpose == Purpose::Terminate {
                    self.close(CloseReason::HostTerminated);
                }
            }
        }
    }

    fn ensure_idle(&self) -> Result<(), ConversationError> {
        match self.phase {
            Phase::Closed => Err(ConversationError::Closed),
            Phase::AwaitingHello | Phase::AwaitingStatus => Err(ConversationError::NotReady),
            _ if self.pending.is_some() => Err(ConversationError::Busy),
            Phase::InTopic(_) => Err(ConversationError::Busy),
            _ => Ok(()),
        }
    }

    fn reply(
        &mut self,
        acked_control_id: &str,
        ack_type: AckType,
        note: Option<&str>,
        now: Now<'_>,
    ) -> Result<(), ConversationError> {
        let control_id = self.next_id();
        let message = builder::ack(
            &self.header(&control_id, now),
            ack_type,
            acked_control_id,
            note,
        )?;
        self.send(message, now.instant);
        Ok(())
    }

    fn send_terminate(&mut self, now: Now<'_>) -> Result<String, ConversationError> {
        let control_id = self.next_id();
        let message = builder::terminate(&self.header(&control_id, now), None)?;
        self.send(message, now.instant);
        Ok(control_id)
    }

    fn expect_ack(&mut self, control_id: String, purpose: Purpose, now: Instant) {
        self.pending = Some(Pending {
            control_id,
            purpose,
            deadline: now + self.config.response_timeout,
        });
    }

    fn send(&mut self, message: Message, now: Instant) {
        self.last_sent = now;
        self.outputs.push_back(Output::Send(message));
    }

    fn close(&mut self, reason: CloseReason) {
        self.phase = Phase::Closed;
        self.pending = None;
        self.awaiting_application.clear();
        self.outputs.push_back(Output::Closed(reason));
    }

    fn next_id(&mut self) -> String {
        let id = self.next_control_id.to_string();
        self.next_control_id = self.next_control_id.wrapping_add(1);
        id
    }

    fn header<'a>(&'a self, control_id: &'a str, now: Now<'a>) -> Header<'a> {
        Header {
            control_id,
            version_id: &self.config.version_id,
            creation_dttm: now.datetime,
        }
    }
}

/// Rejects caller-supplied text that cannot be written in XML, before any
/// state changes.
fn check_text(context: &str, text: &str) -> Result<(), ConversationError> {
    match text.chars().find(|&c| !is_xml_char(c)) {
        Some(c) => Err(BuildError::InvalidCharacter {
            context: context.to_owned(),
            code: u32::from(c),
        }
        .into()),
        None => Ok(()),
    }
}
