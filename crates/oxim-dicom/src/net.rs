//! Helpers shared by the connectors and steps.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use oxim_core::{ConnectorError, EngineError, Settings, SourceContext};
use serde::de::DeserializeOwned;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, warn};

/// How long associations get to finish the object in progress when the
/// channel stops.
const ASSOCIATION_GRACE: Duration = Duration::from_secs(5);

/// Deserializes connector or step settings into a typed struct.
pub(crate) fn settings<T: DeserializeOwned>(
    settings: &Settings,
    what: &str,
) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::Object(settings.clone()))
        .map_err(|e| EngineError::Config(format!("{what} settings: {e}")))
}

/// Checks an application entity title: 1 to 16 characters of printable
/// ASCII without backslash, not only spaces (PS3.5 table 6.2-1).
pub(crate) fn validate_ae_title(what: &str, title: &str) -> Result<(), EngineError> {
    let valid = !title.trim().is_empty()
        && title.len() <= 16
        && title
            .bytes()
            .all(|b| (0x20..0x7F).contains(&b) && b != b'\\');
    if valid {
        Ok(())
    } else {
        Err(EngineError::Config(format!(
            "{what} {title:?} must be 1 to 16 printable ASCII characters without backslash"
        )))
    }
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
                    warn!(channel = %context.channel(), %peer, "association limit reached; closing connection");
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
    // Associations observe the cancellation themselves; give them time to
    // finish the object in progress.
    let drained = tokio::time::timeout(ASSOCIATION_GRACE, async {
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
    fn validates_ae_titles() {
        for good in ["OXIM", "STORE-SCP", "A", "SIXTEEN_CHARS_AE"] {
            assert!(validate_ae_title("AE title", good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "   ",
            "SEVENTEEN_CHARS_A",
            "BACK\\SLASH",
            "TAB\tAE",
            "ÄE",
        ] {
            assert!(validate_ae_title("AE title", bad).is_err(), "{bad}");
        }
    }
}
