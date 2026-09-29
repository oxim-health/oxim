//! Receiving messages from a mailbox folder over IMAP, with async-imap and
//! rustls.
//!
//! Source type `imap`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `host` | required | IMAP server |
//! | `port` | `993` with `security: tls`, else `143` | Server port |
//! | `security` | `tls` | `tls` (implicit TLS), `starttls` (required) or `none` (plain, for trusted local servers only) |
//! | `tls` | system roots | TLS settings; see [`oxim_connectors::tls`] |
//! | `username` | required | User name |
//! | `password_env` | none | Environment variable holding the password |
//! | `password` | none | The password inline (discouraged) |
//! | `folder` | `INBOX` | Folder to read |
//! | `search` | `UNSEEN` | IMAP `SEARCH` criteria selecting the emails to take |
//! | `content` | `message` | `message` (the whole email, RFC 5322) or `attachments` (each matching attachment is a message) |
//! | `attachment_pattern` | `*` | Attachment file names to take; `*` and `?` wildcards |
//! | `after` | `seen` | What happens to a stored email: `seen` (flag it), `delete` or `move` |
//! | `move_to` | none | Folder stored emails are moved to (required for `after: move`) |
//! | `poll_interval` | `60s` | Time between checks |
//! | `max_message_size` | 25 MiB | Larger emails are skipped with a warning |
//! | `connect_timeout` | `10s` | Limit for connecting, TLS and login |
//! | `timeout` | `60s` | Limit for each command |
//!
//! Emails are read with `BODY.PEEK[]`, so they stay unread until they are
//! stored; `after` applies only once every message taken from an email is
//! stored. If OXIM stops in between, the email is read again (at least
//! once). With `content: attachments`, an email without matching
//! attachments is treated as done. Messages carry `mail.subject`,
//! `mail.from`, `mail.message_id` and `imap.uid` metadata, and
//! `file.name` for attachments.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_imap::{Client, Session};
use futures_util::StreamExt;
use mail_parser::{MessageParser, MimeHeaders};
use oxim_connectors::file::matches;
use oxim_connectors::tls::{ClientTlsSettings, client_config, server_name};
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, EngineError, Registry, SourceConfig, SourceConnector, SourceContext,
    SubmitInfo, async_trait,
};
use rustls_pki_types::ServerName;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, info, warn};

use crate::util::{check_secret, parse, secret};

/// How the IMAP connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImapSecurity {
    /// TLS from the first byte.
    #[default]
    Tls,
    /// Plain connection upgraded with `STARTTLS`, which is required.
    Starttls,
    /// No encryption.
    None,
}

/// What becomes a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImapContent {
    /// The whole email.
    #[default]
    Message,
    /// Each matching attachment.
    Attachments,
}

/// What happens to a stored email.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImapAfter {
    /// Flag it as seen.
    #[default]
    Seen,
    /// Delete it.
    Delete,
    /// Move it to `move_to`.
    Move,
}

fn default_folder() -> String {
    "INBOX".to_owned()
}

fn default_search() -> String {
    "UNSEEN".to_owned()
}

fn default_pattern() -> String {
    "*".to_owned()
}

fn default_poll_interval() -> DurationText {
    DurationText(Duration::from_secs(60))
}

fn default_max_message_size() -> u64 {
    25 * 1024 * 1024
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

/// Settings of the `imap` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapSettings {
    /// IMAP server.
    pub host: String,
    /// Server port.
    #[serde(default)]
    pub port: Option<u16>,
    /// Connection protection.
    #[serde(default)]
    pub security: ImapSecurity,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// User name.
    pub username: String,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// The password inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Folder to read.
    #[serde(default = "default_folder")]
    pub folder: String,
    /// Search criteria.
    #[serde(default = "default_search")]
    pub search: String,
    /// What becomes a message.
    #[serde(default)]
    pub content: ImapContent,
    /// Attachment file names to take.
    #[serde(default = "default_pattern")]
    pub attachment_pattern: String,
    /// What happens to a stored email.
    #[serde(default)]
    pub after: ImapAfter,
    /// Folder for `after: move`.
    #[serde(default)]
    pub move_to: Option<String>,
    /// Time between checks.
    #[serde(default = "default_poll_interval")]
    pub poll_interval: DurationText,
    /// Largest accepted email.
    #[serde(default = "default_max_message_size")]
    pub max_message_size: u64,
    /// Limit for connecting and login.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Limit for each command.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

/// A connection, plain or with TLS.
#[derive(Debug)]
enum MailStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for MailStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MailStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Reads mail from an IMAP folder.
pub struct ImapSource {
    settings: ImapSettings,
    port: u16,
    tls: Option<(TlsConnector, ServerName<'static>)>,
}

impl std::fmt::Debug for ImapSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapSource")
            .field("host", &self.settings.host)
            .field("port", &self.port)
            .field("folder", &self.settings.folder)
            .finish_non_exhaustive()
    }
}

/// Characters that would end or split an IMAP command.
fn safe_argument(text: &str) -> bool {
    !text.is_empty() && !text.contains(['\r', '\n', '\0'])
}

type ImapSession = Session<MailStream>;

impl ImapSource {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: ImapSettings) -> Result<Self, EngineError> {
        let error = |message: &str| EngineError::Config(format!("imap: {message}"));
        if settings.host.trim().is_empty() {
            return Err(error("host must not be empty"));
        }
        check_secret(
            settings.password.as_ref(),
            settings.password_env.as_ref(),
            "password",
            "imap",
        )?;
        if settings.password.is_none() && settings.password_env.is_none() {
            return Err(error("set password_env (or password)"));
        }
        if !safe_argument(&settings.folder) || !safe_argument(&settings.search) {
            return Err(error("folder and search must be single-line text"));
        }
        match (&settings.after, &settings.move_to) {
            (ImapAfter::Move, None) => return Err(error("`after: move` needs move_to")),
            (_, Some(folder)) if !safe_argument(folder) => {
                return Err(error("move_to must be single-line text"));
            }
            _ => {}
        }
        if settings.poll_interval.0.is_zero() {
            return Err(error("poll_interval must be positive"));
        }
        let port = settings.port.unwrap_or(match settings.security {
            ImapSecurity::Tls => 993,
            _ => 143,
        });
        let tls = match settings.security {
            ImapSecurity::None => {
                if settings.tls.is_some() {
                    return Err(error("tls settings need security tls or starttls"));
                }
                None
            }
            _ => {
                let tls = settings.tls.clone().unwrap_or_default();
                let name = server_name(&tls, &format!("{}:{port}", settings.host))?;
                Some((TlsConnector::from(Arc::new(client_config(&tls)?)), name))
            }
        };
        Ok(Self {
            settings,
            port,
            tls,
        })
    }

    fn location(&self) -> String {
        let scheme = if self.tls.is_some() { "imaps" } else { "imap" };
        format!(
            "{scheme}://{}@{}:{}/{}",
            self.settings.username, self.settings.host, self.port, self.settings.folder
        )
    }

    async fn upgrade(&self, tcp: TcpStream) -> Result<MailStream, String> {
        let Some((connector, name)) = &self.tls else {
            return Ok(MailStream::Plain(tcp));
        };
        connector
            .connect(name.clone(), tcp)
            .await
            .map(|stream| MailStream::Tls(Box::new(stream)))
            .map_err(|e| format!("TLS handshake failed: {e}"))
    }

    /// Issues `STARTTLS` on a fresh connection, reading the greeting first.
    async fn starttls(tcp: TcpStream) -> Result<TcpStream, String> {
        let mut reader = BufReader::new(tcp);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("no greeting: {e}"))?;
        if !line.starts_with("* OK") {
            return Err(format!("unexpected greeting {:?}", line.trim_end()));
        }
        reader
            .get_mut()
            .write_all(b"S0 STARTTLS\r\n")
            .await
            .map_err(|e| format!("cannot send STARTTLS: {e}"))?;
        loop {
            line.clear();
            if reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("STARTTLS failed: {e}"))?
                == 0
            {
                return Err("the server closed the connection after STARTTLS".into());
            }
            if line.starts_with("S0 OK") {
                break;
            }
            if line.starts_with("S0 ") {
                return Err(format!("the server refused STARTTLS: {}", line.trim_end()));
            }
        }
        if !reader.buffer().is_empty() {
            return Err("unexpected data after STARTTLS".into());
        }
        Ok(reader.into_inner())
    }

    async fn open(&self) -> Result<ImapSession, String> {
        let settings = &self.settings;
        let tcp = TcpStream::connect((settings.host.as_str(), self.port))
            .await
            .map_err(|e| format!("cannot connect: {e}"))?;
        let _ = tcp.set_nodelay(true);
        let (stream, greeted) = match settings.security {
            ImapSecurity::Starttls => (self.upgrade(Self::starttls(tcp).await?).await?, true),
            _ => (self.upgrade(tcp).await?, false),
        };
        let mut client = Client::new(stream);
        if !greeted {
            client
                .read_response()
                .await
                .map_err(|e| format!("no greeting: {e}"))?
                .ok_or("the server closed the connection")?;
        }
        let password = secret(
            settings.password.as_ref(),
            settings.password_env.as_ref(),
            "password",
        )?
        .unwrap_or_default();
        let mut session = client
            .login(&settings.username, &password)
            .await
            .map_err(|(e, _)| format!("login as {} failed: {e}", settings.username))?;
        session
            .select(&settings.folder)
            .await
            .map_err(|e| format!("cannot open folder {}: {e}", settings.folder))?;
        Ok(session)
    }

    /// Fetches one email.
    async fn fetch(&self, session: &mut ImapSession, uid: u32) -> Result<Option<Vec<u8>>, String> {
        let mut fetches = session
            .uid_fetch(uid.to_string(), "(UID RFC822.SIZE BODY.PEEK[])")
            .await
            .map_err(|e| format!("fetching {uid} failed: {e}"))?;
        let mut body = None;
        while let Some(fetch) = fetches.next().await {
            let fetch = fetch.map_err(|e| format!("fetching {uid} failed: {e}"))?;
            if fetch.uid == Some(uid)
                && let Some(data) = fetch.body()
            {
                body = Some(data.to_vec());
            }
        }
        Ok(body)
    }

    /// The messages one email yields, with their metadata.
    fn messages(&self, uid: u32, raw: Vec<u8>) -> Vec<(Vec<u8>, BTreeMap<String, String>)> {
        let mut metadata = BTreeMap::from([("imap.uid".to_owned(), uid.to_string())]);
        let parsed = MessageParser::default().parse(&raw);
        if let Some(message) = &parsed {
            if let Some(subject) = message.subject() {
                metadata.insert("mail.subject".to_owned(), subject.to_owned());
            }
            if let Some(from) = message
                .from()
                .and_then(|from| from.first())
                .and_then(|a| a.address())
            {
                metadata.insert("mail.from".to_owned(), from.to_owned());
            }
            if let Some(id) = message.message_id() {
                metadata.insert("mail.message_id".to_owned(), id.to_owned());
            }
        }
        match self.settings.content {
            ImapContent::Message => vec![(raw, metadata)],
            ImapContent::Attachments => parsed
                .map(|message| {
                    message
                        .attachments()
                        .filter_map(|part| {
                            let name = part.attachment_name()?.to_owned();
                            matches(&self.settings.attachment_pattern, &name).then(|| {
                                let mut metadata = metadata.clone();
                                metadata.insert("file.name".to_owned(), name);
                                (part.contents().to_vec(), metadata)
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    async fn finish(&self, session: &mut ImapSession, uid: u32) -> Result<(), String> {
        let settings = &self.settings;
        let uid = uid.to_string();
        match settings.after {
            ImapAfter::Seen => {
                let updates = session
                    .uid_store(&uid, "+FLAGS (\\Seen)")
                    .await
                    .map_err(|e| format!("flagging {uid} failed: {e}"))?;
                let _: Vec<_> = updates.collect().await;
            }
            ImapAfter::Delete => {
                let updates = session
                    .uid_store(&uid, "+FLAGS (\\Seen \\Deleted)")
                    .await
                    .map_err(|e| format!("deleting {uid} failed: {e}"))?;
                let _: Vec<_> = updates.collect().await;
            }
            ImapAfter::Move => {
                let folder = settings.move_to.as_deref().unwrap_or_default();
                // async-imap quotes the folder name.
                session
                    .uid_mv(&uid, folder)
                    .await
                    .map_err(|e| format!("moving {uid} to {folder} failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// One check of the folder. Returns an error when the session broke.
    async fn poll(
        &self,
        context: &SourceContext,
        session: &mut ImapSession,
        oversized: &mut HashSet<u32>,
    ) -> Result<(), String> {
        let settings = &self.settings;
        let mut uids: Vec<u32> = session
            .uid_search(&settings.search)
            .await
            .map_err(|e| format!("search failed: {e}"))?
            .into_iter()
            .collect();
        uids.sort_unstable();
        let mut deleted = false;
        for uid in uids {
            if context.is_cancelled() {
                break;
            }
            if oversized.contains(&uid) {
                continue;
            }
            let Some(raw) = self.fetch(session, uid).await? else {
                continue;
            };
            if raw.len() as u64 > settings.max_message_size {
                oversized.insert(uid);
                warn!(channel = %context.channel(), location = %self.location(), uid, size = raw.len(), "email too large; skipping it");
                continue;
            }
            let messages = self.messages(uid, raw);
            if messages.is_empty() {
                debug!(channel = %context.channel(), uid, "no matching attachment");
            }
            let mut stored = true;
            for (data, metadata) in messages {
                let info = SubmitInfo {
                    peer: Some(self.location()),
                    correlation_id: metadata.get("mail.message_id").cloned(),
                    metadata,
                    ..SubmitInfo::default()
                };
                if let Err(e) = context.submit(data, info).await {
                    warn!(channel = %context.channel(), uid, error = %e, "email could not be stored; will retry");
                    stored = false;
                    break;
                }
            }
            if !stored {
                break;
            }
            self.finish(session, uid).await?;
            deleted |= settings.after == ImapAfter::Delete;
        }
        if deleted {
            let expunged = session
                .expunge()
                .await
                .map_err(|e| format!("expunge failed: {e}"))?;
            let _: Vec<_> = expunged.collect().await;
        }
        Ok(())
    }
}

#[async_trait]
impl SourceConnector for ImapSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let location = self.location();
        info!(channel = %context.channel(), %location, "watching mailbox");
        let mut oversized = HashSet::new();
        let mut failing = false;
        loop {
            let opened = tokio::time::timeout(self.settings.connect_timeout.0, self.open())
                .await
                .map_err(|_| "connecting timed out".to_owned())
                .and_then(|result| result);
            match opened {
                Ok(mut session) => {
                    failing = false;
                    let polled = tokio::time::timeout(
                        self.settings.timeout.0.saturating_mul(4),
                        self.poll(&context, &mut session, &mut oversized),
                    )
                    .await
                    .unwrap_or_else(|_| Err("the check timed out".into()));
                    if let Err(e) = polled {
                        warn!(channel = %context.channel(), %location, error = %e, "mailbox check failed");
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(5), session.logout()).await;
                }
                Err(e) => {
                    if !failing {
                        warn!(channel = %context.channel(), %location, error = %e, "cannot connect; retrying every poll interval");
                    }
                    failing = true;
                }
            }
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                () = tokio::time::sleep(self.settings.poll_interval.0) => {}
            }
        }
    }
}

/// Registers the `imap` source.
pub fn register(registry: &mut Registry) {
    registry.add_source("imap", |config: &SourceConfig| {
        let settings: ImapSettings = parse(&config.settings, "imap source")?;
        Ok(Arc::new(ImapSource::new(settings)?) as Arc<dyn SourceConnector>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(extra: &str) -> Result<ImapSource, EngineError> {
        let mut settings: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{"host":"mail.lab.test","username":"lab","password_env":"OXIM_IMAP"}"#,
        )
        .unwrap();
        let extra: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&format!("{{{extra}}}")).unwrap();
        settings.extend(extra);
        ImapSource::new(serde_json::from_value(serde_json::Value::Object(settings)).unwrap())
    }

    #[test]
    fn checks_settings() {
        assert_eq!(source("").unwrap().port, 993);
        assert_eq!(source(r#""security":"starttls""#).unwrap().port, 143);
        assert!(source(r#""after":"move""#).is_err());
        assert!(source(r#""after":"move","move_to":"Done""#).is_ok());
        assert!(source(r#""search":"UNSEEN\r\nA1 DELETE INBOX""#).is_err());
        assert!(source(r#""security":"none","tls":{}"#).is_err());
        assert_eq!(
            source(r#""security":"none""#).unwrap().location(),
            "imap://lab@mail.lab.test:143/INBOX"
        );
    }

    #[test]
    fn extracts_attachments() {
        let email = b"From: Analyzer <analyzer@lab.test>\r\n\
Subject: Results\r\n\
Message-ID: <r1@lab.test>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
See attached.\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
Content-Disposition: attachment; filename=\"results.hl7\"\r\n\
\r\n\
MSH|^~\\&|A\r\n\
--b\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERg==\r\n\
--b--\r\n";
        let attachments =
            source(r#""content":"attachments","attachment_pattern":"*.hl7""#).unwrap();
        let messages = attachments.messages(7, email.to_vec());
        assert_eq!(messages.len(), 1);
        assert!(messages[0].0.starts_with(b"MSH|"));
        assert_eq!(messages[0].1["file.name"], "results.hl7");
        assert_eq!(messages[0].1["mail.subject"], "Results");
        assert_eq!(messages[0].1["mail.from"], "analyzer@lab.test");
        assert_eq!(messages[0].1["imap.uid"], "7");
        let whole = source("").unwrap().messages(7, email.to_vec());
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].0, email);
    }
}
