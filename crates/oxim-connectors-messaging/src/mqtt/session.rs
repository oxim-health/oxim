//! One MQTT connection: connect, send and receive packets.

use std::time::Duration;

use oxim_connectors::tls::{Dialer, Stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::codec::{Decoder, Packet, encode};

/// What a client sends in `CONNECT`.
#[derive(Debug, Clone)]
pub(crate) struct Login {
    pub(crate) client_id: String,
    pub(crate) clean_session: bool,
    pub(crate) keep_alive: Duration,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
}

/// An established connection.
#[derive(Debug)]
pub(crate) struct Session {
    stream: Stream,
    decoder: Decoder,
    next_id: u16,
}

fn refused(code: u8) -> String {
    match code {
        1 => "the broker does not support MQTT 3.1.1".into(),
        2 => "the broker rejected the client identifier".into(),
        3 => "the MQTT service is unavailable".into(),
        4 => "bad user name or password".into(),
        5 => "not authorized".into(),
        other => format!("connection refused with code {other}"),
    }
}

impl Session {
    /// Connects and logs in; returns whether the broker resumed a session.
    pub(crate) async fn open(
        dialer: &Dialer,
        login: &Login,
        timeout: Duration,
        max_packet: usize,
    ) -> Result<(Self, bool), String> {
        let stream = dialer.connect().await?;
        let mut session = Self {
            stream,
            decoder: Decoder::new(max_packet),
            next_id: 0,
        };
        let keep_alive = u16::try_from(login.keep_alive.as_secs()).unwrap_or(u16::MAX);
        session
            .send(&Packet::Connect {
                client_id: login.client_id.clone(),
                clean_session: login.clean_session,
                keep_alive,
                username: login.username.clone(),
                password: login.password.clone().map(String::into_bytes),
            })
            .await?;
        match tokio::time::timeout(timeout, session.read())
            .await
            .map_err(|_| "the broker did not acknowledge the connection".to_owned())??
        {
            Packet::ConnAck {
                session_present,
                code: 0,
            } => Ok((session, session_present)),
            Packet::ConnAck { code, .. } => Err(refused(code)),
            other => Err(format!("expected CONNACK, got {other:?}")),
        }
    }

    /// A fresh packet identifier (never 0).
    pub(crate) fn packet_id(&mut self) -> u16 {
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.next_id
    }

    /// Sends a packet.
    pub(crate) async fn send(&mut self, packet: &Packet) -> Result<(), String> {
        let bytes = encode(packet)?;
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|e| format!("sending failed: {e}"))?;
        self.stream
            .flush()
            .await
            .map_err(|e| format!("sending failed: {e}"))
    }

    /// Reads the next packet. Cancel-safe: bytes already received stay in
    /// the decoder.
    pub(crate) async fn read(&mut self) -> Result<Packet, String> {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            if let Some(packet) = self.decoder.next()? {
                return Ok(packet);
            }
            let n = self
                .stream
                .read(&mut buffer)
                .await
                .map_err(|e| format!("receiving failed: {e}"))?;
            if n == 0 {
                return Err("the broker closed the connection".into());
            }
            self.decoder.push(&buffer[..n]);
        }
    }

    /// Ends the session cleanly.
    pub(crate) async fn close(mut self) {
        let _ = self.send(&Packet::Disconnect).await;
        let _ = self.stream.shutdown().await;
    }
}
