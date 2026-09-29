//! MQTT 3.1.1 packets (OASIS standard), the subset a client with QoS 0 and
//! 1 needs: `CONNECT`, `CONNACK`, `PUBLISH`, `PUBACK`, `SUBSCRIBE`,
//! `SUBACK`, `PINGREQ`, `PINGRESP` and `DISCONNECT`. Sans-IO: bytes in,
//! packets out.

/// The largest packet accepted by default: 16 MiB.
pub(crate) const DEFAULT_MAX_PACKET: usize = 16 * 1024 * 1024;

/// One MQTT control packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Packet {
    /// Client connection request.
    Connect {
        /// Client identifier.
        client_id: String,
        /// Whether the broker discards the previous session.
        clean_session: bool,
        /// Keep-alive interval in seconds.
        keep_alive: u16,
        /// User name.
        username: Option<String>,
        /// Password.
        password: Option<Vec<u8>>,
    },
    /// Connection acknowledgment.
    ConnAck {
        /// Whether the broker resumed a stored session.
        session_present: bool,
        /// Return code; 0 is accepted.
        code: u8,
    },
    /// An application message.
    Publish {
        /// Redelivery of an unacknowledged message.
        dup: bool,
        /// Quality of service (0, 1 or 2).
        qos: u8,
        /// Whether the broker keeps it for new subscribers.
        retain: bool,
        /// Topic name.
        topic: String,
        /// Packet identifier, for QoS 1 and 2.
        packet_id: Option<u16>,
        /// Application payload.
        payload: Vec<u8>,
    },
    /// Acknowledgment of a QoS 1 publish.
    PubAck(u16),
    /// Subscription request.
    Subscribe {
        /// Packet identifier.
        packet_id: u16,
        /// Topic filters with their maximum QoS.
        filters: Vec<(String, u8)>,
    },
    /// Subscription acknowledgment.
    SubAck {
        /// Packet identifier.
        packet_id: u16,
        /// Granted QoS per filter, or `0x80` for a failure.
        codes: Vec<u8>,
    },
    /// Keep-alive request.
    PingReq,
    /// Keep-alive response.
    PingResp,
    /// Clean disconnection.
    Disconnect,
    /// A packet this client does not use (QoS 2 flow, unsubscribe).
    Other(u8),
}

fn put_str(out: &mut Vec<u8>, text: &[u8]) -> Result<(), String> {
    let len = u16::try_from(text.len()).map_err(|_| "a string longer than 65535 bytes")?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(text);
    Ok(())
}

fn put_remaining_length(out: &mut Vec<u8>, mut len: usize) -> Result<(), String> {
    if len > 268_435_455 {
        return Err("the packet is larger than MQTT allows".into());
    }
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            return Ok(());
        }
    }
}

/// Encodes a packet.
pub(crate) fn encode(packet: &Packet) -> Result<Vec<u8>, String> {
    let (header, body) = match packet {
        Packet::Connect {
            client_id,
            clean_session,
            keep_alive,
            username,
            password,
        } => {
            let mut body = Vec::new();
            put_str(&mut body, b"MQTT")?;
            body.push(4); // protocol level 3.1.1
            let mut flags = 0u8;
            if *clean_session {
                flags |= 0x02;
            }
            if username.is_some() {
                flags |= 0x80;
            }
            if password.is_some() {
                flags |= 0x40;
            }
            body.push(flags);
            body.extend_from_slice(&keep_alive.to_be_bytes());
            put_str(&mut body, client_id.as_bytes())?;
            if let Some(username) = username {
                put_str(&mut body, username.as_bytes())?;
            }
            if let Some(password) = password {
                put_str(&mut body, password)?;
            }
            (0x10, body)
        }
        Packet::ConnAck {
            session_present,
            code,
        } => (0x20, vec![u8::from(*session_present), *code]),
        Packet::Publish {
            dup,
            qos,
            retain,
            topic,
            packet_id,
            payload,
        } => {
            if *qos > 2 {
                return Err(format!("invalid QoS {qos}"));
            }
            let mut header = 0x30 | (qos << 1);
            if *dup {
                header |= 0x08;
            }
            if *retain {
                header |= 0x01;
            }
            let mut body = Vec::with_capacity(topic.len() + payload.len() + 4);
            put_str(&mut body, topic.as_bytes())?;
            if *qos > 0 {
                let id = packet_id.ok_or("a QoS 1 or 2 publish needs a packet identifier")?;
                body.extend_from_slice(&id.to_be_bytes());
            }
            body.extend_from_slice(payload);
            (header, body)
        }
        Packet::PubAck(id) => (0x40, id.to_be_bytes().to_vec()),
        Packet::Subscribe { packet_id, filters } => {
            let mut body = packet_id.to_be_bytes().to_vec();
            for (filter, qos) in filters {
                put_str(&mut body, filter.as_bytes())?;
                body.push(*qos);
            }
            (0x82, body)
        }
        Packet::SubAck { packet_id, codes } => {
            let mut body = packet_id.to_be_bytes().to_vec();
            body.extend_from_slice(codes);
            (0x90, body)
        }
        Packet::PingReq => (0xC0, Vec::new()),
        Packet::PingResp => (0xD0, Vec::new()),
        Packet::Disconnect => (0xE0, Vec::new()),
        Packet::Other(kind) => return Err(format!("cannot encode packet type {kind}")),
    };
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(header);
    put_remaining_length(&mut out, body.len())?;
    out.extend_from_slice(&body);
    Ok(out)
}

/// A reader over a packet body.
struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Body<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let byte = *self.bytes.get(self.at).ok_or("a truncated packet")?;
        self.at += 1;
        Ok(byte)
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    fn bytes(&mut self) -> Result<&[u8], String> {
        let len = usize::from(self.u16()?);
        let slice = self
            .bytes
            .get(self.at..self.at + len)
            .ok_or("a truncated packet")?;
        self.at += len;
        Ok(slice)
    }

    fn string(&mut self) -> Result<String, String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| "a string that is not UTF-8".into())
    }

    fn rest(&self) -> &[u8] {
        self.bytes.get(self.at..).unwrap_or_default()
    }
}

fn decode_body(header: u8, bytes: &[u8]) -> Result<Packet, String> {
    let kind = header >> 4;
    let flags = header & 0x0F;
    let mut body = Body { bytes, at: 0 };
    Ok(match kind {
        1 => {
            if body.bytes()? != b"MQTT" || body.u8()? != 4 {
                return Err("only MQTT 3.1.1 is supported".into());
            }
            let connect_flags = body.u8()?;
            let keep_alive = body.u16()?;
            let client_id = body.string()?;
            if connect_flags & 0x04 != 0 {
                // A will message: topic and payload, not used here.
                body.bytes()?;
                body.bytes()?;
            }
            let username = if connect_flags & 0x80 != 0 {
                Some(body.string()?)
            } else {
                None
            };
            let password = if connect_flags & 0x40 != 0 {
                Some(body.bytes()?.to_vec())
            } else {
                None
            };
            Packet::Connect {
                client_id,
                clean_session: connect_flags & 0x02 != 0,
                keep_alive,
                username,
                password,
            }
        }
        2 => Packet::ConnAck {
            session_present: body.u8()? & 0x01 != 0,
            code: body.u8()?,
        },
        3 => {
            let qos = (flags >> 1) & 0x03;
            if qos == 3 {
                return Err("a publish with QoS 3".into());
            }
            let topic = body.string()?;
            let packet_id = if qos > 0 { Some(body.u16()?) } else { None };
            Packet::Publish {
                dup: flags & 0x08 != 0,
                qos,
                retain: flags & 0x01 != 0,
                topic,
                packet_id,
                payload: body.rest().to_vec(),
            }
        }
        4 => Packet::PubAck(body.u16()?),
        8 => {
            let packet_id = body.u16()?;
            let mut filters = Vec::new();
            while !body.rest().is_empty() {
                filters.push((body.string()?, body.u8()?));
            }
            Packet::Subscribe { packet_id, filters }
        }
        9 => Packet::SubAck {
            packet_id: body.u16()?,
            codes: body.rest().to_vec(),
        },
        12 => Packet::PingReq,
        13 => Packet::PingResp,
        14 => Packet::Disconnect,
        0 | 15 => return Err(format!("reserved packet type {kind}")),
        other => Packet::Other(other),
    })
}

/// Splits a byte stream into packets.
#[derive(Debug)]
pub(crate) struct Decoder {
    buffer: Vec<u8>,
    max_packet: usize,
}

impl Decoder {
    pub(crate) fn new(max_packet: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_packet,
        }
    }

    /// Adds received bytes.
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete packet, if any. An error means the stream is
    /// corrupt and the connection must be closed.
    pub(crate) fn next(&mut self) -> Result<Option<Packet>, String> {
        let Some(&header) = self.buffer.first() else {
            return Ok(None);
        };
        let mut len = 0usize;
        let mut multiplier = 1usize;
        let mut at = 1;
        loop {
            let Some(&byte) = self.buffer.get(at) else {
                return Ok(None);
            };
            len += usize::from(byte & 0x7F) * multiplier;
            at += 1;
            if byte & 0x80 == 0 {
                break;
            }
            multiplier *= 128;
            if at > 4 {
                return Err("a malformed remaining length".into());
            }
        }
        if len > self.max_packet {
            return Err(format!(
                "a packet of {len} bytes is larger than the limit of {}",
                self.max_packet
            ));
        }
        if self.buffer.len() < at + len {
            return Ok(None);
        }
        let packet = decode_body(header, &self.buffer[at..at + len]);
        self.buffer.drain(..at + len);
        packet.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn round_trip(packet: Packet) {
        let bytes = encode(&packet).unwrap();
        let mut decoder = Decoder::new(DEFAULT_MAX_PACKET);
        // Byte by byte, to exercise partial input.
        for byte in &bytes[..bytes.len() - 1] {
            decoder.push(&[*byte]);
            assert_eq!(decoder.next().unwrap(), None);
        }
        decoder.push(&bytes[bytes.len() - 1..]);
        assert_eq!(decoder.next().unwrap(), Some(packet));
        assert_eq!(decoder.next().unwrap(), None);
    }

    #[test]
    fn packets_round_trip() {
        round_trip(Packet::Connect {
            client_id: "oxim-lab".into(),
            clean_session: false,
            keep_alive: 30,
            username: Some("lab".into()),
            password: Some(b"not-a-real-secret".to_vec()),
        });
        round_trip(Packet::ConnAck {
            session_present: true,
            code: 0,
        });
        round_trip(Packet::Publish {
            dup: true,
            qos: 1,
            retain: false,
            topic: "lab/results".into(),
            packet_id: Some(7),
            payload: vec![0; 300],
        });
        round_trip(Packet::Publish {
            dup: false,
            qos: 0,
            retain: true,
            topic: "t".into(),
            packet_id: None,
            payload: Vec::new(),
        });
        round_trip(Packet::PubAck(65535));
        round_trip(Packet::Subscribe {
            packet_id: 1,
            filters: vec![("lab/#".into(), 1), ("poct/+".into(), 0)],
        });
        round_trip(Packet::SubAck {
            packet_id: 1,
            codes: vec![1, 0x80],
        });
        round_trip(Packet::PingReq);
        round_trip(Packet::PingResp);
        round_trip(Packet::Disconnect);
    }

    #[test]
    fn encodes_known_bytes() {
        assert_eq!(encode(&Packet::PingReq).unwrap(), [0xC0, 0x00]);
        assert_eq!(
            encode(&Packet::PubAck(0x0102)).unwrap(),
            [0x40, 0x02, 0x01, 0x02]
        );
        let publish = encode(&Packet::Publish {
            dup: false,
            qos: 1,
            retain: false,
            topic: "a".into(),
            packet_id: Some(1),
            payload: vec![0xAB; 200],
        })
        .unwrap();
        // Remaining length 205 = 0xCD 0x01 in two bytes.
        assert_eq!(&publish[..3], [0x32, 0xCD, 0x01]);
    }

    #[test]
    fn rejects_malformed_input() {
        let mut decoder = Decoder::new(16);
        decoder.push(&[0x30, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]);
        assert!(decoder.next().is_err());
        let mut decoder = Decoder::new(4);
        decoder.push(&[0x30, 0x05]);
        assert!(decoder.next().is_err());
        let mut decoder = Decoder::new(64);
        decoder.push(&[0x36, 0x00]); // QoS 3
        assert!(decoder.next().is_err());
        assert!(
            encode(&Packet::Publish {
                dup: false,
                qos: 1,
                retain: false,
                topic: "t".into(),
                packet_id: None,
                payload: Vec::new()
            })
            .is_err()
        );
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let mut decoder = Decoder::new(1024);
            decoder.push(&bytes);
            for _ in 0..64 {
                match decoder.next() {
                    Ok(Some(_)) => continue,
                    _ => break,
                }
            }
        }
    }
}
