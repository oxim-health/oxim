//! Sans-IO ASTM E1381 (CLSI LIS01-A2) link sessions.
//!
//! A [`Session`] drives one half-duplex LIS01 link. It receives messages the
//! other side sends and sends the messages queued with [`Session::send`],
//! handling the establishment, transfer and termination phases, frame
//! acknowledgments, retransmissions, timeouts, contention and receiver
//! interrupts.
//!
//! The session never touches the network, a serial port or the clock. Feed it
//! received bytes with [`Session::handle_input`], call
//! [`Session::handle_timeout`] when [`Session::poll_timeout`] expires, and act
//! on every [`Output`] from [`Session::poll_output`]: write
//! [`Output::Transmit`] bytes to the link and process received messages.
//!
//! ```
//! use std::time::{Duration, Instant};
//! use oxim_astm::session::{Output, Session, SessionConfig};
//! use oxim_astm::frame::{ACK, ENQ, EOT, Frame};
//!
//! let now = Instant::now();
//! let mut session = Session::new(SessionConfig::default());
//! session.handle_input(&[ENQ], now);
//! assert_eq!(session.poll_output(), Some(Output::Transmit(vec![ACK])));
//!
//! let frame = Frame::new(1, b"H|\\^&\r".to_vec(), true)?;
//! session.handle_input(&frame.encode(), now);
//! assert_eq!(session.poll_output(), Some(Output::Transmit(vec![ACK])));
//!
//! session.handle_input(&[EOT], now + Duration::from_secs(1));
//! assert_eq!(session.poll_output(), Some(Output::Received(b"H|\\^&\r".to_vec())));
//! # Ok::<(), oxim_astm::FrameError>(())
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use crate::error::{FrameError, SendError};
use crate::frame::{
    ACK, Decoded, ENQ, EOT, Frame, NAK, STX, decode_frame, is_restricted, split_message,
};

/// Which side of the link this session plays. It decides who yields when
/// both sides request the link at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The laboratory instrument. It has priority on contention and retries
    /// its request after a short delay.
    Instrument,
    /// The computer system (LIS or OXIM acting as one). It yields on
    /// contention.
    Host,
}

/// Timers and limits of a [`Session`]. The defaults are the values LIS01-A2
/// prescribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionConfig {
    /// This side of the link. Defaults to [`Role::Host`].
    pub role: Role,
    /// How long the sender waits for a reply to ENQ or to a frame (15 s).
    pub reply_timeout: Duration,
    /// How long the receiver waits for the next frame or EOT (30 s).
    pub receive_timeout: Duration,
    /// How long the sender waits after its ENQ was refused with NAK, or went
    /// unanswered, before requesting the link again (10 s).
    pub enq_retry_delay: Duration,
    /// How long the host waits after contention before requesting the link
    /// again (20 s).
    pub host_contention_delay: Duration,
    /// How long the instrument waits after contention before requesting the
    /// link again (1 s).
    pub instrument_contention_delay: Duration,
    /// How long the sender stays quiet after the receiver requested an
    /// interrupt, giving the other side the link (15 s).
    pub interrupt_delay: Duration,
    /// How many times a refused frame is retransmitted before the message is
    /// aborted (6).
    pub max_retransmissions: u32,
    /// The largest frame text sent or accepted (240 bytes).
    pub max_frame_text: usize,
    /// The largest message accepted in either direction.
    pub max_message_len: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            role: Role::Host,
            reply_timeout: Duration::from_secs(15),
            receive_timeout: Duration::from_secs(30),
            enq_retry_delay: Duration::from_secs(10),
            host_contention_delay: Duration::from_secs(20),
            instrument_contention_delay: Duration::from_secs(1),
            interrupt_delay: Duration::from_secs(15),
            max_retransmissions: 6,
            max_frame_text: crate::frame::MAX_FRAME_TEXT,
            max_message_len: 16 * 1024 * 1024,
        }
    }
}

/// Identifies a message queued with [`Session::send`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u64);

impl MessageId {
    /// The numeric value, unique within one session.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Why a queued message was abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AbortReason {
    /// A frame was refused more often than
    /// [`SessionConfig::max_retransmissions`] allows.
    TooManyRetransmissions,
    /// The receiver did not reply to a frame in time.
    ReplyTimeout,
}

/// Something the session reports for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// The other side refused our ENQ with NAK; the request is retried
    /// later.
    EnqRefused,
    /// The other side did not answer our ENQ; the request is retried later.
    EnqUnanswered,
    /// Both sides requested the link at the same time.
    Contention,
    /// We refused the other side's ENQ because the session is busy.
    BusyRefused,
    /// A received frame was invalid and refused with NAK.
    InvalidFrame(FrameError),
    /// A frame was received again after its acknowledgment was lost; it was
    /// acknowledged and not appended twice.
    DuplicateFrame {
        /// The repeated frame number.
        number: u8,
    },
    /// A frame arrived with an unexpected number and was refused.
    UnexpectedFrameNumber {
        /// The number that was expected.
        expected: u8,
        /// The number that arrived.
        received: u8,
    },
    /// A frame is being retransmitted after it was refused.
    Retransmission {
        /// The number of retransmissions of this frame so far.
        attempt: u32,
    },
    /// The receiver asked us to stop sending (EOT instead of ACK). The
    /// current message is completed, then the link is released.
    InterruptRequested,
    /// The sender went quiet; the partial message was discarded.
    ReceiveTimeout {
        /// Bytes of text discarded.
        discarded: usize,
    },
    /// The sender ended the transmission in the middle of a message; the
    /// partial message was discarded.
    IncompleteMessage {
        /// Bytes of text discarded.
        discarded: usize,
    },
    /// A received message exceeded [`SessionConfig::max_message_len`]; it
    /// was acknowledged frame by frame and discarded.
    MessageTooLarge {
        /// Bytes of text received.
        received: usize,
    },
    /// Bytes that mean nothing in the current state were ignored.
    Ignored {
        /// How many bytes.
        bytes: usize,
    },
}

/// Everything a [`Session`] asks its caller to do or know.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Output {
    /// Bytes to write to the link, in order.
    Transmit(Vec<u8>),
    /// The text of a complete received transmission: the concatenated text
    /// of every frame between ENQ and EOT, normally one ASTM E1394 message
    /// (`H` ... `L`) with CR-terminated records.
    Received(Vec<u8>),
    /// Every frame of a queued message was acknowledged.
    Delivered(MessageId),
    /// A queued message was abandoned. It is not retried automatically.
    Aborted {
        /// The message.
        id: MessageId,
        /// Why.
        reason: AbortReason,
    },
    /// A diagnostic event.
    Event(Event),
}

#[derive(Debug)]
struct Outgoing {
    id: MessageId,
    text: Vec<u8>,
}

#[derive(Debug)]
struct Transfer {
    id: MessageId,
    frames: Vec<Frame>,
    index: usize,
    retransmissions: u32,
    deadline: Instant,
    interrupted: bool,
}

#[derive(Debug)]
struct Reception {
    expected: u8,
    last: Option<u8>,
    text: Vec<u8>,
    received: usize,
    complete: bool,
    overflow: bool,
    deadline: Instant,
}

#[derive(Debug)]
enum State {
    /// The link is free.
    Neutral,
    /// The link is free but we may not request it before `until`.
    Holding { until: Instant },
    /// We sent ENQ and wait for the reply.
    Establishing { deadline: Instant },
    /// We are sending a message.
    Transferring(Transfer),
    /// We are receiving a message.
    Receiving(Reception),
}

/// One half-duplex LIS01 link. See the [module documentation](self).
#[derive(Debug)]
pub struct Session {
    config: SessionConfig,
    state: State,
    input: Vec<u8>,
    outputs: VecDeque<Output>,
    queue: VecDeque<Outgoing>,
    next_id: u64,
    busy: bool,
}

impl Session {
    /// Creates a session with a free link.
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            state: State::Neutral,
            input: Vec::new(),
            outputs: VecDeque::new(),
            queue: VecDeque::new(),
            next_id: 0,
            busy: false,
        }
    }

    /// The configuration.
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// While busy, the session refuses the other side's ENQ with NAK, as
    /// LIS01 allows a receiver that cannot accept a message.
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    /// Whether the link is free and nothing is queued.
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Neutral | State::Holding { .. }) && self.queue.is_empty()
    }

    /// The number of queued messages, excluding one being sent.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Queues an ASTM E1394 message for sending. Records must be terminated
    /// by CR; the text must not contain characters LIS01 reserves for link
    /// control (including LF).
    pub fn send(&mut self, message: Vec<u8>, now: Instant) -> Result<MessageId, SendError> {
        if message.is_empty() {
            return Err(SendError::Empty);
        }
        if message.len() > self.config.max_message_len {
            return Err(SendError::TooLarge);
        }
        if let Some(position) = message.iter().position(|&b| is_restricted(b)) {
            return Err(SendError::RestrictedCharacter {
                byte: message[position],
                position,
            });
        }
        let id = MessageId(self.next_id);
        self.next_id += 1;
        self.queue.push_back(Outgoing { id, text: message });
        self.try_start(now);
        Ok(id)
    }

    /// Feeds bytes received from the link.
    pub fn handle_input(&mut self, bytes: &[u8], now: Instant) {
        self.input.extend_from_slice(bytes);
        while let Some(&byte) = self.input.first() {
            let progressed = match self.state {
                State::Neutral | State::Holding { .. } => self.input_while_free(byte, now),
                State::Establishing { .. } => self.input_while_establishing(byte, now),
                State::Transferring(_) => self.input_while_transferring(byte, now),
                State::Receiving(_) => self.input_while_receiving(byte, now),
            };
            if !progressed {
                break;
            }
        }
    }

    /// The instant at which [`Session::handle_timeout`] must be called
    /// next, if any.
    pub fn poll_timeout(&self) -> Option<Instant> {
        match &self.state {
            State::Neutral => None,
            State::Holding { until } => Some(*until),
            State::Establishing { deadline } => Some(*deadline),
            State::Transferring(transfer) => Some(transfer.deadline),
            State::Receiving(reception) => Some(reception.deadline),
        }
    }

    /// Advances timers. Safe to call at any time.
    pub fn handle_timeout(&mut self, now: Instant) {
        match &self.state {
            State::Holding { until } if now >= *until => {
                self.state = State::Neutral;
                self.try_start(now);
            }
            State::Establishing { deadline } if now >= *deadline => {
                // LIS01: no reply to ENQ ends the attempt with EOT.
                self.transmit(vec![EOT]);
                self.event(Event::EnqUnanswered);
                self.hold(now + self.config.enq_retry_delay);
            }
            State::Transferring(transfer) if now >= transfer.deadline => {
                let id = transfer.id;
                self.transmit(vec![EOT]);
                self.outputs.push_back(Output::Aborted {
                    id,
                    reason: AbortReason::ReplyTimeout,
                });
                self.state = State::Neutral;
                self.try_start(now);
            }
            State::Receiving(reception) if now >= reception.deadline => {
                let discarded = reception.text.len();
                self.event(Event::ReceiveTimeout { discarded });
                self.state = State::Neutral;
                self.try_start(now);
            }
            _ => {}
        }
    }

    /// The next output, if any. Call until it returns `None` after every
    /// other method call.
    pub fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }

    fn input_while_free(&mut self, byte: u8, now: Instant) -> bool {
        if byte != ENQ {
            self.ignore_until(&[ENQ]);
            return true;
        }
        self.input.drain(..1);
        self.accept_enq(now);
        true
    }

    fn input_while_establishing(&mut self, byte: u8, now: Instant) -> bool {
        match byte {
            ACK => {
                self.input.drain(..1);
                self.start_transfer(now);
            }
            NAK => {
                self.input.drain(..1);
                self.event(Event::EnqRefused);
                self.hold(now + self.config.enq_retry_delay);
            }
            ENQ => {
                self.input.drain(..1);
                self.event(Event::Contention);
                match self.config.role {
                    // The host yields and waits for the instrument's next
                    // ENQ, which it accepts while holding.
                    Role::Host => self.hold(now + self.config.host_contention_delay),
                    // The instrument keeps priority and asks again shortly.
                    Role::Instrument => self.hold(now + self.config.instrument_contention_delay),
                }
            }
            _ => self.ignore_until(&[ACK, NAK, ENQ]),
        }
        true
    }

    fn input_while_transferring(&mut self, byte: u8, now: Instant) -> bool {
        match byte {
            ACK => {
                self.input.drain(..1);
                self.frame_accepted(now);
            }
            EOT => {
                self.input.drain(..1);
                self.event(Event::InterruptRequested);
                if let State::Transferring(transfer) = &mut self.state {
                    transfer.interrupted = true;
                }
                // LIS01: EOT in place of ACK acknowledges the frame.
                self.frame_accepted(now);
            }
            NAK => {
                self.input.drain(..1);
                self.frame_refused(now);
            }
            // Only ACK, NAK and EOT are replies. Other bytes, such as stray
            // line breaks, are ignored instead of being treated as NAK so
            // that noise cannot cause retransmissions; a lost reply is
            // covered by the reply timeout.
            _ => self.ignore_until(&[ACK, NAK, EOT]),
        }
        true
    }

    fn input_while_receiving(&mut self, byte: u8, now: Instant) -> bool {
        match byte {
            STX => match decode_frame(&self.input, self.config.max_frame_text) {
                Decoded::Incomplete => return false,
                Decoded::Frame { frame, len } => {
                    self.input.drain(..len);
                    self.frame_received(frame, now);
                }
                Decoded::Invalid { error, len } => {
                    self.input.drain(..len);
                    self.event(Event::InvalidFrame(error));
                    self.transmit(vec![NAK]);
                    self.touch_reception(now);
                }
            },
            EOT => {
                self.input.drain(..1);
                self.finish_reception(now);
            }
            ENQ => {
                // The sender started over; drop what we have and accept anew.
                self.input.drain(..1);
                if let State::Receiving(reception) = &self.state
                    && !reception.text.is_empty()
                {
                    let discarded = reception.text.len();
                    self.event(Event::IncompleteMessage { discarded });
                }
                self.state = State::Neutral;
                self.accept_enq(now);
            }
            _ => self.ignore_until(&[STX, EOT, ENQ]),
        }
        true
    }

    fn accept_enq(&mut self, now: Instant) {
        if self.busy {
            self.transmit(vec![NAK]);
            self.event(Event::BusyRefused);
            return;
        }
        self.transmit(vec![ACK]);
        self.state = State::Receiving(Reception {
            expected: 1,
            last: None,
            text: Vec::new(),
            received: 0,
            complete: false,
            overflow: false,
            deadline: now + self.config.receive_timeout,
        });
    }

    fn frame_received(&mut self, frame: Frame, now: Instant) {
        let max = self.config.max_message_len;
        let State::Receiving(reception) = &mut self.state else {
            return;
        };
        let (reply, event) = if frame.number() == reception.expected {
            reception.received += frame.text().len();
            if !reception.overflow {
                if reception.text.len() + frame.text().len() > max {
                    reception.overflow = true;
                    reception.text = Vec::new();
                } else {
                    reception.text.extend_from_slice(frame.text());
                }
            }
            reception.last = Some(frame.number());
            reception.expected = (frame.number() + 1) % 8;
            reception.complete = frame.is_last();
            (ACK, None)
        } else if Some(frame.number()) == reception.last {
            (
                ACK,
                Some(Event::DuplicateFrame {
                    number: frame.number(),
                }),
            )
        } else {
            (
                NAK,
                Some(Event::UnexpectedFrameNumber {
                    expected: reception.expected,
                    received: frame.number(),
                }),
            )
        };
        reception.deadline = now + self.config.receive_timeout;
        if let Some(event) = event {
            self.event(event);
        }
        self.transmit(vec![reply]);
    }

    fn touch_reception(&mut self, now: Instant) {
        if let State::Receiving(reception) = &mut self.state {
            reception.deadline = now + self.config.receive_timeout;
        }
    }

    fn finish_reception(&mut self, now: Instant) {
        let state = std::mem::replace(&mut self.state, State::Neutral);
        if let State::Receiving(reception) = state {
            if reception.overflow {
                self.event(Event::MessageTooLarge {
                    received: reception.received,
                });
            } else if !reception.text.is_empty() {
                if reception.complete {
                    self.outputs.push_back(Output::Received(reception.text));
                } else {
                    self.event(Event::IncompleteMessage {
                        discarded: reception.text.len(),
                    });
                }
            }
        }
        self.try_start(now);
    }

    fn start_transfer(&mut self, now: Instant) {
        let Some(outgoing) = self.queue.pop_front() else {
            // Nothing left to send: release the link.
            self.transmit(vec![EOT]);
            self.state = State::Neutral;
            return;
        };
        match split_message(&outgoing.text, 1, self.config.max_frame_text) {
            Ok(frames) if !frames.is_empty() => {
                self.transmit(frames[0].encode());
                self.state = State::Transferring(Transfer {
                    id: outgoing.id,
                    frames,
                    index: 0,
                    retransmissions: 0,
                    deadline: now + self.config.reply_timeout,
                    interrupted: false,
                });
            }
            // `send` validated the text, so this cannot happen; release the
            // link rather than panic.
            _ => {
                self.transmit(vec![EOT]);
                self.state = State::Neutral;
                self.try_start(now);
            }
        }
    }

    fn frame_accepted(&mut self, now: Instant) {
        let reply_timeout = self.config.reply_timeout;
        let State::Transferring(transfer) = &mut self.state else {
            return;
        };
        transfer.index += 1;
        transfer.retransmissions = 0;
        if let Some(frame) = transfer.frames.get(transfer.index) {
            let bytes = frame.encode();
            transfer.deadline = now + reply_timeout;
            self.transmit(bytes);
            return;
        }
        let id = transfer.id;
        let interrupted = transfer.interrupted;
        self.outputs.push_back(Output::Delivered(id));
        self.transmit(vec![EOT]);
        if interrupted {
            self.hold(now + self.config.interrupt_delay);
        } else {
            self.state = State::Neutral;
            self.try_start(now);
        }
    }

    fn frame_refused(&mut self, now: Instant) {
        let config = self.config;
        let State::Transferring(transfer) = &mut self.state else {
            return;
        };
        if transfer.retransmissions >= config.max_retransmissions {
            let id = transfer.id;
            self.transmit(vec![EOT]);
            self.outputs.push_back(Output::Aborted {
                id,
                reason: AbortReason::TooManyRetransmissions,
            });
            self.state = State::Neutral;
            self.try_start(now);
            return;
        }
        transfer.retransmissions += 1;
        transfer.deadline = now + config.reply_timeout;
        let attempt = transfer.retransmissions;
        let bytes = transfer
            .frames
            .get(transfer.index)
            .map(Frame::encode)
            .unwrap_or_default();
        self.event(Event::Retransmission { attempt });
        self.transmit(bytes);
    }

    /// Requests the link if the session is free and something is queued.
    fn try_start(&mut self, now: Instant) {
        if self.queue.is_empty() {
            return;
        }
        match self.state {
            State::Neutral => {
                self.transmit(vec![ENQ]);
                self.state = State::Establishing {
                    deadline: now + self.config.reply_timeout,
                };
            }
            State::Holding { until } if now >= until => {
                self.state = State::Neutral;
                self.try_start(now);
            }
            _ => {}
        }
    }

    fn hold(&mut self, until: Instant) {
        self.state = State::Holding { until };
    }

    /// Drops input bytes up to the next byte in `stop`.
    fn ignore_until(&mut self, stop: &[u8]) {
        let len = self
            .input
            .iter()
            .position(|b| stop.contains(b))
            .unwrap_or(self.input.len());
        if len > 0 {
            self.input.drain(..len);
            self.event(Event::Ignored { bytes: len });
        }
    }

    fn transmit(&mut self, bytes: Vec<u8>) {
        self.outputs.push_back(Output::Transmit(bytes));
    }

    fn event(&mut self, event: Event) {
        self.outputs.push_back(Output::Event(event));
    }
}
