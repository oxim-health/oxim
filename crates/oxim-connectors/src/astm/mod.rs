//! ASTM E1381/E1394 (CLSI LIS01/LIS02) connectors for laboratory analyzers.
//!
//! | Type | Kind | Transport |
//! |---|---|---|
//! | `astm-tcp` | source, destination | LIS01 over TCP, as server or client |
//! | `astm-serial` | source, destination | LIS01 over a serial port |
//! | `astm-raw-tcp` | source | ASTM records over TCP without LIS01 framing |
//!
//! LIS01 sources acknowledge the frame that completes a message only after
//! the message is stored durably; if storing fails the frame is refused and
//! the analyzer retransmits it (ADR 0004). An analyzer both sends results
//! and receives worklists over one link, so an `astm-tcp` or `astm-serial`
//! source and destination with the same endpoint share one link (see
//! [`link`](self) for details); their link settings must then be identical.

mod link;
mod raw;

use std::sync::Arc;
use std::time::Duration;

use oxim_astm::session::{Role, SessionConfig};
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConnector, EngineError, Registry, SendError, Settings,
    SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_store::Delivery;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tracing::warn;

pub use raw::{AstmRawTcpSettings, AstmRawTcpSource};

use self::link::{Link, LinkOptions, SendFailure, Transport};
use crate::serial::{FlowControl, Parity, PortSettings};

/// Parses connector settings into their typed form.
fn parse<T: DeserializeOwned>(kind: &str, settings: &Settings) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::Object(settings.clone()))
        .map_err(|e| EngineError::Config(format!("{kind} settings: {e}")))
}

/// Whether a TCP connector listens or connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Listen on `listen` for the device to connect.
    #[default]
    Server,
    /// Connect to the device at `connect`.
    Client,
}

/// Which LIS01 side OXIM plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    /// The computer system (LIS); it yields when both sides request the
    /// link. Use this when talking to an analyzer.
    #[default]
    Host,
    /// The instrument; it keeps priority on contention. Use this when OXIM
    /// stands in for an instrument, for example towards another LIS.
    Instrument,
}

fn duration(seconds: u64) -> DurationText {
    DurationText(Duration::from_secs(seconds))
}

fn reconnect_delay_default() -> DurationText {
    duration(1)
}

fn max_reconnect_delay_default() -> DurationText {
    duration(60)
}

fn confirmation_timeout_default() -> DurationText {
    duration(10)
}

fn send_timeout_default() -> DurationText {
    duration(120)
}

/// Settings of the `astm-tcp` source and destination.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AstmTcpSettings {
    /// `server` (default) or `client`.
    #[serde(default)]
    pub mode: Mode,
    /// Address to listen on in server mode, for example `0.0.0.0:5100`.
    #[serde(default)]
    pub listen: Option<String>,
    /// Address to connect to in client mode, for example `10.0.0.5:5100`.
    #[serde(default)]
    pub connect: Option<String>,
    /// LIS01 role, `host` by default.
    #[serde(default)]
    pub role: LinkRole,
    /// First delay before reconnecting or listening again after a failure.
    #[serde(default = "reconnect_delay_default")]
    pub reconnect_delay: DurationText,
    /// Upper bound of the reconnect delay.
    #[serde(default = "max_reconnect_delay_default")]
    pub max_reconnect_delay: DurationText,
    /// How long the final acknowledgment of a received message waits for the
    /// message to be stored; must stay below the 15-second LIS01 reply
    /// timeout.
    #[serde(default = "confirmation_timeout_default")]
    pub confirmation_timeout: DurationText,
    /// Destination only: how long one delivery may take, including waiting
    /// for the device to connect.
    #[serde(default = "send_timeout_default")]
    pub send_timeout: DurationText,
    /// Largest message accepted or sent, in bytes (16 MiB by default).
    #[serde(default)]
    pub max_message_len: Option<usize>,
}

/// Settings of the `astm-serial` source and destination.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AstmSerialSettings {
    /// Port name, for example `COM3` or `/dev/ttyUSB0`.
    pub port: String,
    /// Baud rate (9600).
    #[serde(default = "baud_rate_default")]
    pub baud_rate: u32,
    /// Data bits (8).
    #[serde(default = "data_bits_default")]
    pub data_bits: u8,
    /// Parity (`none`, `odd`, `even`).
    #[serde(default)]
    pub parity: Parity,
    /// Stop bits (1).
    #[serde(default = "stop_bits_default")]
    pub stop_bits: u8,
    /// Flow control (`none`, `software`, `hardware`).
    #[serde(default)]
    pub flow_control: FlowControl,
    /// LIS01 role, `host` by default.
    #[serde(default)]
    pub role: LinkRole,
    /// First delay before reopening the port after a failure.
    #[serde(default = "reconnect_delay_default")]
    pub reopen_delay: DurationText,
    /// Upper bound of the reopen delay.
    #[serde(default = "max_reconnect_delay_default")]
    pub max_reopen_delay: DurationText,
    /// See [`AstmTcpSettings::confirmation_timeout`].
    #[serde(default = "confirmation_timeout_default")]
    pub confirmation_timeout: DurationText,
    /// See [`AstmTcpSettings::send_timeout`].
    #[serde(default = "send_timeout_default")]
    pub send_timeout: DurationText,
    /// Largest message accepted or sent, in bytes (16 MiB by default).
    #[serde(default)]
    pub max_message_len: Option<usize>,
}

fn baud_rate_default() -> u32 {
    9600
}

fn data_bits_default() -> u8 {
    8
}

fn stop_bits_default() -> u8 {
    1
}

fn session_config(
    role: LinkRole,
    confirmation_timeout: DurationText,
    max_message_len: Option<usize>,
) -> Result<SessionConfig, EngineError> {
    let mut config = SessionConfig::default();
    if confirmation_timeout.0.is_zero() || confirmation_timeout.0 >= config.reply_timeout {
        return Err(EngineError::Config(format!(
            "confirmation_timeout must be between 0 and {} s, the LIS01 reply timeout",
            config.reply_timeout.as_secs()
        )));
    }
    config.role = match role {
        LinkRole::Host => Role::Host,
        LinkRole::Instrument => Role::Instrument,
    };
    config.defer_final_ack = true;
    config.confirmation_timeout = confirmation_timeout.0;
    if let Some(max) = max_message_len {
        if max == 0 {
            return Err(EngineError::Config(
                "max_message_len must be positive".into(),
            ));
        }
        config.max_message_len = max;
    }
    Ok(config)
}

fn delays(initial: DurationText, max: DurationText) -> Result<(Duration, Duration), EngineError> {
    if initial.0.is_zero() || initial.0 > max.0 {
        return Err(EngineError::Config(
            "the reconnect delay must be positive and not exceed its maximum".into(),
        ));
    }
    Ok((initial.0, max.0))
}

impl AstmTcpSettings {
    fn link(&self) -> Result<(Transport, LinkOptions, Duration), EngineError> {
        let transport = match (self.mode, &self.listen, &self.connect) {
            (Mode::Server, Some(listen), None) => Transport::TcpListen(listen.clone()),
            (Mode::Client, None, Some(connect)) => Transport::TcpConnect(connect.clone()),
            (Mode::Server, _, _) => {
                return Err(EngineError::Config(
                    "astm-tcp in server mode needs `listen` and no `connect`".into(),
                ));
            }
            (Mode::Client, _, _) => {
                return Err(EngineError::Config(
                    "astm-tcp in client mode needs `connect` and no `listen`".into(),
                ));
            }
        };
        let (reconnect_delay, max_reconnect_delay) =
            delays(self.reconnect_delay, self.max_reconnect_delay)?;
        let options = LinkOptions {
            session: session_config(self.role, self.confirmation_timeout, self.max_message_len)?,
            reconnect_delay,
            max_reconnect_delay,
        };
        Ok((transport, options, self.send_timeout.0))
    }
}

impl AstmSerialSettings {
    fn link(&self) -> Result<(Transport, LinkOptions, Duration), EngineError> {
        let port = PortSettings {
            port: self.port.clone(),
            baud_rate: self.baud_rate,
            data_bits: self.data_bits,
            parity: self.parity,
            stop_bits: self.stop_bits,
            flow_control: self.flow_control,
        };
        port.validate()
            .map_err(|e| EngineError::Config(format!("astm-serial settings: {e}")))?;
        let (reconnect_delay, max_reconnect_delay) =
            delays(self.reopen_delay, self.max_reopen_delay)?;
        let options = LinkOptions {
            session: session_config(self.role, self.confirmation_timeout, self.max_message_len)?,
            reconnect_delay,
            max_reconnect_delay,
        };
        Ok((Transport::Serial(port), options, self.send_timeout.0))
    }
}

/// Receives messages from an analyzer over a shared LIS01 link and stores
/// them before acknowledging their final frame.
#[derive(Debug)]
pub struct AstmSource {
    link: Arc<Link>,
}

#[async_trait]
impl SourceConnector for AstmSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let (sender, mut inbox) = mpsc::channel(8);
        let _attachment = self.link.attach(sender).map_err(ConnectorError)?;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                inbound = inbox.recv() => {
                    let Some(inbound) = inbound else {
                        return Err(ConnectorError(format!("ASTM link {} stopped", self.link.key())));
                    };
                    let info = SubmitInfo {
                        peer: Some(inbound.peer),
                        ..SubmitInfo::default()
                    };
                    let stored = context.submit(inbound.text, info).await;
                    if let Err(error) = &stored {
                        warn!(link = self.link.key(), %error, "cannot store an ASTM message; the analyzer will retransmit it");
                    }
                    if let Some(confirm) = inbound.confirm {
                        let _ = confirm.send(stored.is_ok());
                    }
                }
            }
        }
    }
}

/// Sends messages, typically worklists, to an analyzer over a shared LIS01
/// link.
#[derive(Debug)]
pub struct AstmDestination {
    link: Arc<Link>,
    send_timeout: Duration,
}

#[async_trait]
impl DestinationConnector for AstmDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        match self
            .link
            .send(delivery.payload.clone(), self.send_timeout)
            .await
        {
            Ok(()) => Ok(None),
            Err(SendFailure::Invalid(message)) => Err(SendError::permanent(message)),
            Err(SendFailure::Failed(message)) => Err(SendError::temporary(message)),
        }
    }
}

/// Registers `astm-tcp`, `astm-serial` and `astm-raw-tcp`.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("astm-tcp", |config| {
            let settings: AstmTcpSettings = parse("astm-tcp", &config.settings)?;
            let (transport, options, _) = settings.link()?;
            Ok(Arc::new(AstmSource {
                link: link::acquire(transport, options)?,
            }) as Arc<dyn SourceConnector>)
        })
        .add_destination("astm-tcp", |config| {
            let settings: AstmTcpSettings = parse("astm-tcp", &config.settings)?;
            let (transport, options, send_timeout) = settings.link()?;
            Ok(Arc::new(AstmDestination {
                link: link::acquire(transport, options)?,
                send_timeout,
            }) as Arc<dyn DestinationConnector>)
        })
        .add_source("astm-serial", |config| {
            let settings: AstmSerialSettings = parse("astm-serial", &config.settings)?;
            let (transport, options, _) = settings.link()?;
            Ok(Arc::new(AstmSource {
                link: link::acquire(transport, options)?,
            }) as Arc<dyn SourceConnector>)
        })
        .add_destination("astm-serial", |config| {
            let settings: AstmSerialSettings = parse("astm-serial", &config.settings)?;
            let (transport, options, send_timeout) = settings.link()?;
            Ok(Arc::new(AstmDestination {
                link: link::acquire(transport, options)?,
                send_timeout,
            }) as Arc<dyn DestinationConnector>)
        })
        .add_source("astm-raw-tcp", |config| {
            let settings: AstmRawTcpSettings = parse("astm-raw-tcp", &config.settings)?;
            Ok(Arc::new(AstmRawTcpSource::new(settings)?) as Arc<dyn SourceConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: serde_json::Value) -> Settings {
        match json {
            serde_json::Value::Object(map) => map,
            _ => Settings::new(),
        }
    }

    #[test]
    fn validates_tcp_settings() {
        let ok: AstmTcpSettings = parse(
            "astm-tcp",
            &settings(serde_json::json!({"listen": "127.0.0.1:5100"})),
        )
        .unwrap();
        let (transport, options, send_timeout) = ok.link().unwrap();
        assert_eq!(transport, Transport::TcpListen("127.0.0.1:5100".into()));
        assert!(options.session.defer_final_ack);
        assert_eq!(options.session.role, Role::Host);
        assert_eq!(send_timeout, Duration::from_secs(120));

        for bad in [
            serde_json::json!({}),
            serde_json::json!({"mode": "client", "listen": "127.0.0.1:1"}),
            serde_json::json!({"listen": "a", "connect": "b"}),
            serde_json::json!({"listen": "a", "confirmation_timeout": "15s"}),
            serde_json::json!({"listen": "a", "reconnect_delay": "2m", "max_reconnect_delay": "1m"}),
            serde_json::json!({"listen": "a", "max_message_len": 0}),
        ] {
            let parsed: Result<AstmTcpSettings, _> = parse("astm-tcp", &settings(bad.clone()));
            assert!(parsed.and_then(|s| s.link()).is_err(), "{bad}");
        }
        assert!(
            parse::<AstmTcpSettings>(
                "astm-tcp",
                &settings(serde_json::json!({"listen": "a", "unknown": 1}))
            )
            .is_err()
        );
    }

    #[test]
    fn validates_serial_settings() {
        let serial: AstmSerialSettings = parse(
            "astm-serial",
            &settings(serde_json::json!({"port": "COM3", "baud_rate": 19200, "parity": "even", "role": "instrument"})),
        )
        .unwrap();
        let (transport, options, _) = serial.link().unwrap();
        assert_eq!(transport.key(), "serial:COM3");
        assert_eq!(options.session.role, Role::Instrument);
        let bad: AstmSerialSettings = parse(
            "astm-serial",
            &settings(serde_json::json!({"port": "COM3", "data_bits": 9})),
        )
        .unwrap();
        assert!(bad.link().is_err());
    }
}
