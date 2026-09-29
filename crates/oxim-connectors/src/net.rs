//! Helpers shared by the connectors.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use oxim_core::{ConnectorError, EngineError, Settings, SourceContext};
use oxim_model::{ClinicalDateTime, Timestamp};
use serde::de::DeserializeOwned;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, warn};

/// The default bound for one message: 16 MiB.
pub(crate) const DEFAULT_MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// How long connections get to finish their current message when the
/// channel stops.
const CONNECTION_GRACE: Duration = Duration::from_secs(5);

/// Deserializes connector settings into a typed struct.
pub(crate) fn settings<T: DeserializeOwned>(
    settings: &Settings,
    what: &str,
) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::Object(settings.clone()))
        .map_err(|e| EngineError::Config(format!("{what} settings: {e}")))
}

/// An instant as an HL7 `DTM` in UTC, to the second: `20260929143005+0000`.
pub(crate) fn hl7_timestamp(timestamp: Timestamp) -> String {
    match ClinicalDateTime::from_timestamp(timestamp, 0) {
        Some(value) => format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}+0000",
            value.year(),
            value.month().unwrap_or(1),
            value.day().unwrap_or(1),
            value.hour().unwrap_or(0),
            value.minute().unwrap_or(0),
            value.second().unwrap_or(0),
        ),
        None => "19700101000000+0000".to_owned(),
    }
}

/// Parses a byte sequence written in configuration: either `hex:` followed
/// by hexadecimal digits (`hex:0B`), or text with the escapes `\r`, `\n`,
/// `\t`, `\\` and `\xHH` (`"\x1c\r"`).
pub(crate) fn parse_bytes(text: &str) -> Result<Vec<u8>, String> {
    if let Some(hex) = text.strip_prefix("hex:") {
        let digits: Vec<u8> = hex.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        if digits.is_empty() || !digits.len().is_multiple_of(2) {
            return Err(format!("invalid hexadecimal byte sequence {text:?}"));
        }
        return digits
            .chunks(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or_else(|| format!("invalid hexadecimal byte sequence {text:?}"))
            })
            .collect();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buffer = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                let byte = u8::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 2)
                    .ok_or_else(|| format!("invalid escape \\x{hex} in {text:?}"))?;
                out.push(byte);
            }
            other => {
                return Err(format!(
                    "invalid escape \\{} in {text:?}",
                    other.map(String::from).unwrap_or_default()
                ));
            }
        }
    }
    if out.is_empty() {
        return Err("a byte sequence must not be empty".into());
    }
    Ok(out)
}

/// Accepts TCP connections on `listen` until the channel stops, running
/// `handle` for each one. Connections beyond `max_connections` are closed
/// immediately.
pub(crate) async fn serve<F, Fut>(
    context: &SourceContext,
    listen: &str,
    max_connections: usize,
    handle: F,
) -> Result<(), ConnectorError>
where
    F: Fn(TcpStream, SocketAddr) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind(listen)
        .await
        .map_err(|e| ConnectorError(format!("cannot listen on {listen}: {e}")))?;
    debug!(channel = %context.channel(), %listen, "listening");
    let permits = Arc::new(Semaphore::new(max_connections.max(1)));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = context.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        warn!(channel = %context.channel(), error = %e, "accept failed");
                        continue;
                    }
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    warn!(channel = %context.channel(), %peer, "connection limit reached; closing connection");
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let connection = handle(stream, peer);
                connections.spawn(async move {
                    connection.await;
                    drop(permit);
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    // Connections observe the cancellation themselves; give them time to
    // finish the message in progress.
    let drained = tokio::time::timeout(CONNECTION_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        connections.shutdown().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_byte_sequences() {
        assert_eq!(parse_bytes("hex:0B").unwrap(), [0x0b]);
        assert_eq!(parse_bytes("hex:1c 0d").unwrap(), [0x1c, 0x0d]);
        assert_eq!(parse_bytes("\\x1c\\r").unwrap(), [0x1c, 0x0d]);
        assert_eq!(parse_bytes("END\\n").unwrap(), b"END\n");
        for bad in ["", "hex:", "hex:0", "hex:zz", "\\q", "\\x1", "\\xZZ"] {
            assert!(parse_bytes(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn formats_hl7_timestamps() {
        let ts = Timestamp::from_unix_millis(1_790_067_600_250).unwrap();
        assert_eq!(hl7_timestamp(ts), "20260922090000+0000");
    }
}
