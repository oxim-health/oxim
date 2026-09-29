//! A simulated analyzer and host over ASTM LIS01 (TCP).

use std::io;
use std::time::Instant;

use oxim_astm::session::{Output, Role, Session, SessionConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// What happened to messages sent with [`send`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AstmReport {
    /// Messages queued.
    pub queued: u64,
    /// Messages acknowledged frame by frame.
    pub delivered: u64,
    /// Messages abandoned by the session.
    pub aborted: u64,
    /// Messages received from the other side.
    pub received: u64,
}

/// Writes pending session output and records what it reports.
async fn flush(
    stream: &mut TcpStream,
    session: &mut Session,
    report: &mut AstmReport,
    on_received: &mut impl FnMut(&[u8]),
) -> io::Result<()> {
    while let Some(output) = session.poll_output() {
        match output {
            Output::Transmit(bytes) => stream.write_all(&bytes).await?,
            Output::Received(message) => {
                report.received += 1;
                on_received(&message);
            }
            Output::Delivered(_) => report.delivered += 1,
            Output::Aborted { .. } => report.aborted += 1,
            _ => {}
        }
    }
    Ok(())
}

/// Waits for input or the next session timer. Returns `false` when the
/// connection closed.
async fn wait(stream: &mut TcpStream, session: &mut Session) -> io::Result<bool> {
    let mut buffer = [0u8; 4096];
    let read = match session.poll_timeout() {
        Some(deadline) => {
            let deadline = tokio::time::Instant::from_std(deadline);
            tokio::select! {
                read = stream.read(&mut buffer) => Some(read),
                () = tokio::time::sleep_until(deadline) => None,
            }
        }
        None => Some(stream.read(&mut buffer).await),
    };
    match read {
        Some(Ok(0)) => Ok(false),
        Some(Ok(n)) => {
            session.handle_input(&buffer[..n], Instant::now());
            Ok(true)
        }
        Some(Err(e)) => Err(e),
        None => {
            session.handle_timeout(Instant::now());
            Ok(true)
        }
    }
}

/// Sends `messages` as an analyzer (instrument role) and returns when every
/// message was delivered or aborted.
pub async fn send(
    mut stream: TcpStream,
    messages: Vec<Vec<u8>>,
    mut on_received: impl FnMut(&[u8]),
) -> io::Result<AstmReport> {
    let mut config = SessionConfig::default();
    config.role = Role::Instrument;
    let mut session = Session::new(config);
    let mut report = AstmReport::default();
    for message in messages {
        session
            .send(message, Instant::now())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        report.queued += 1;
    }
    loop {
        flush(&mut stream, &mut session, &mut report, &mut on_received).await?;
        // Stop once everything is settled; the final EOT was written above.
        if report.delivered + report.aborted >= report.queued && session.is_idle() {
            return Ok(report);
        }
        if !wait(&mut stream, &mut session).await? {
            return Ok(report);
        }
    }
}

/// Receives messages as a host until the connection closes.
pub async fn receive(
    mut stream: TcpStream,
    mut on_received: impl FnMut(&[u8]),
) -> io::Result<AstmReport> {
    let mut session = Session::new(SessionConfig::default());
    let mut report = AstmReport::default();
    loop {
        flush(&mut stream, &mut session, &mut report, &mut on_received).await?;
        if !wait(&mut stream, &mut session).await? {
            flush(&mut stream, &mut session, &mut report, &mut on_received).await?;
            return Ok(report);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::Generator;

    #[tokio::test]
    async fn analyzer_and_host_exchange_messages() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let host = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut messages = Vec::new();
            receive(stream, |message| messages.push(message.to_vec()))
                .await
                .unwrap();
            messages
        });
        let mut generator = Generator::new(3);
        let sent: Vec<_> = (0..3).map(|_| generator.astm_results(5)).collect();
        let stream = TcpStream::connect(address).await.unwrap();
        let report = send(stream, sent.clone(), |_| {}).await.unwrap();
        assert_eq!(report.delivered, 3);
        let received = host.await.unwrap();
        assert_eq!(received, sent);
    }
}
