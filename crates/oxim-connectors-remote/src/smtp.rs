//! Sending messages by email over SMTP, with lettre and rustls.
//!
//! Destination type `smtp`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `host` | required | SMTP server (relay or submission server) |
//! | `port` | `587`, `465` with `security: tls`, `25` with `security: none` | Server port |
//! | `security` | `starttls` | `starttls` (required, never falls back to plain text), `tls` (implicit TLS) or `none` (plain, for trusted local relays only) |
//! | `tls` | system roots | TLS settings; see [`oxim_connectors::tls`] |
//! | `username` | none | User for SMTP authentication |
//! | `password_env` | none | Environment variable holding the password |
//! | `password` | none | The password inline (discouraged) |
//! | `from` | required | Sender, for example `OXIM <oxim@lab.example.org>` |
//! | `to` | required | Recipients (a list) |
//! | `cc` | none | Copy recipients (a list) |
//! | `subject` | `OXIM message {message_id}` | Subject template |
//! | `content` | `attachment` | `attachment` (the message is attached) or `body` (the message is the text body; it must be UTF-8) |
//! | `attachment_name` | `{channel}-{message_id}.{extension}` | File name template of the attachment |
//! | `text` | `Message {message_id} from channel {channel}.` | Text body template when the message is attached |
//! | `hello_name` | the host name | Name sent with `EHLO` |
//! | `timeout` | `30s` | Limit for one delivery |
//!
//! Templates may use `{channel}`, `{destination}`, `{message_id}`,
//! `{timestamp}` and `{extension}`. The `Message-ID` header is derived from
//! the OXIM message identifier, so a delivery repeated after a lost answer
//! can be recognized as a duplicate by the receiving system.
//!
//! Permanent SMTP errors (5xx, for example an unknown recipient) fail the
//! delivery; authentication failures (530, 535) and all other errors are
//! retried, since fixing the credentials lets queued messages through.

use std::sync::Arc;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{
    Certificate, CertificateStore, Identity, Tls, TlsParameters,
};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use oxim_connectors::file::render;
use oxim_connectors::http::content_type;
use oxim_connectors::tls::ClientTlsSettings;
use oxim_core::config::DurationText;
use oxim_core::{
    DestinationConfig, DestinationConnector, EngineError, Registry, SendError, async_trait,
};
use oxim_model::MessageId;
use oxim_store::Delivery;
use serde::Deserialize;

use crate::util::{check_secret, parse, render_text, secret};

/// How the SMTP connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    /// Plain connection upgraded with `STARTTLS`, which is required.
    #[default]
    Starttls,
    /// TLS from the first byte.
    Tls,
    /// No encryption.
    None,
}

/// How the message is put in the email.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// As an attachment.
    #[default]
    Attachment,
    /// As the text body.
    Body,
}

fn default_subject() -> String {
    "OXIM message {message_id}".to_owned()
}

fn default_attachment_name() -> String {
    "{channel}-{message_id}.{extension}".to_owned()
}

fn default_text() -> String {
    "Message {message_id} from channel {channel}.".to_owned()
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

/// Settings of the `smtp` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpSettings {
    /// SMTP server.
    pub host: String,
    /// Server port.
    #[serde(default)]
    pub port: Option<u16>,
    /// Connection protection.
    #[serde(default)]
    pub security: SmtpSecurity,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// User for authentication.
    #[serde(default)]
    pub username: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// The password inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Sender.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// Copy recipients.
    #[serde(default)]
    pub cc: Vec<String>,
    /// Subject template.
    #[serde(default = "default_subject")]
    pub subject: String,
    /// Where the message goes.
    #[serde(default)]
    pub content: Content,
    /// Attachment file name template.
    #[serde(default = "default_attachment_name")]
    pub attachment_name: String,
    /// Text body template when the message is attached.
    #[serde(default = "default_text")]
    pub text: String,
    /// Name sent with `EHLO`.
    #[serde(default)]
    pub hello_name: Option<String>,
    /// Limit for one delivery.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

/// Sends each delivery as an email.
pub struct SmtpDestination {
    settings: SmtpSettings,
    port: u16,
    from: Mailbox,
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    tls: Option<TlsParameters>,
}

impl std::fmt::Debug for SmtpDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpDestination")
            .field("host", &self.settings.host)
            .field("port", &self.port)
            .field("security", &self.settings.security)
            .finish_non_exhaustive()
    }
}

fn mailbox(text: &str, what: &str) -> Result<Mailbox, EngineError> {
    text.parse()
        .map_err(|e| EngineError::Config(format!("smtp: invalid {what} address {text:?}: {e}")))
}

fn tls_parameters(settings: &ClientTlsSettings, host: &str) -> Result<TlsParameters, EngineError> {
    let error = |message: String| EngineError::Config(format!("smtp TLS: {message}"));
    let domain = settings
        .server_name
        .clone()
        .unwrap_or_else(|| host.to_owned());
    let mut builder = TlsParameters::builder(domain).certificate_store(if settings.system_roots {
        CertificateStore::Default
    } else {
        CertificateStore::None
    });
    if let Some(path) = &settings.ca_file {
        let pem = std::fs::read(path)
            .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
        builder = builder.add_root_certificate(
            Certificate::from_pem(&pem)
                .map_err(|e| error(format!("invalid certificate in {}: {e}", path.display())))?,
        );
    }
    match (&settings.cert_file, &settings.key_file) {
        (Some(cert), Some(key)) => {
            let cert_pem = std::fs::read(cert)
                .map_err(|e| error(format!("cannot read {}: {e}", cert.display())))?;
            let key_pem = std::fs::read(key)
                .map_err(|e| error(format!("cannot read {}: {e}", key.display())))?;
            builder = builder.identify_with(
                Identity::from_pem(&cert_pem, &key_pem)
                    .map_err(|e| error(format!("client certificate: {e}")))?,
            );
        }
        (None, None) => {}
        _ => return Err(error("cert_file and key_file must be set together".into())),
    }
    builder.build().map_err(|e| error(e.to_string()))
}

impl SmtpDestination {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: SmtpSettings) -> Result<Self, EngineError> {
        if settings.host.trim().is_empty() {
            return Err(EngineError::Config("smtp: host must not be empty".into()));
        }
        if settings.to.is_empty() {
            return Err(EngineError::Config(
                "smtp: `to` needs at least one recipient".into(),
            ));
        }
        check_secret(
            settings.password.as_ref(),
            settings.password_env.as_ref(),
            "password",
            "smtp",
        )?;
        if settings.username.is_none()
            && (settings.password.is_some() || settings.password_env.is_some())
        {
            return Err(EngineError::Config(
                "smtp: a password needs a username".into(),
            ));
        }
        let probe = MessageId::from_parts(0, 0);
        for template in [&settings.subject, &settings.text] {
            render_text(template, "channel", "destination", probe, None)
                .map_err(|e| EngineError::Config(format!("smtp: {e}")))?;
        }
        render(
            &settings.attachment_name,
            "channel",
            "destination",
            probe,
            None,
        )
        .map_err(|e| EngineError::Config(format!("smtp attachment_name: {e}")))?;
        let from = mailbox(&settings.from, "from")?;
        let to = settings
            .to
            .iter()
            .map(|address| mailbox(address, "to"))
            .collect::<Result<_, _>>()?;
        let cc = settings
            .cc
            .iter()
            .map(|address| mailbox(address, "cc"))
            .collect::<Result<_, _>>()?;
        let port = settings.port.unwrap_or(match settings.security {
            SmtpSecurity::Starttls => 587,
            SmtpSecurity::Tls => 465,
            SmtpSecurity::None => 25,
        });
        let tls = match settings.security {
            SmtpSecurity::None => {
                if settings.tls.is_some() {
                    return Err(EngineError::Config(
                        "smtp: tls settings need security starttls or tls".into(),
                    ));
                }
                None
            }
            _ => Some(tls_parameters(
                &settings.tls.clone().unwrap_or_default(),
                &settings.host,
            )?),
        };
        Ok(Self {
            settings,
            port,
            from,
            to,
            cc,
            tls,
        })
    }

    fn email(&self, delivery: &Delivery) -> Result<Message, SendError> {
        let settings = &self.settings;
        let text = |template: &str| {
            render_text(
                template,
                delivery.channel.as_str(),
                delivery.destination.as_str(),
                delivery.message_id,
                delivery.data_type,
            )
            .map_err(SendError::permanent)
        };
        let mut builder = Message::builder()
            .from(self.from.clone())
            .subject(text(&settings.subject)?)
            .message_id(Some(format!(
                "<{}.{}@oxim.invalid>",
                delivery.message_id, delivery.destination
            )))
            .date_now();
        for recipient in &self.to {
            builder = builder.to(recipient.clone());
        }
        for recipient in &self.cc {
            builder = builder.cc(recipient.clone());
        }
        let built = match settings.content {
            Content::Body => {
                let body = String::from_utf8(delivery.payload.clone()).map_err(|_| {
                    SendError::permanent("the message is not UTF-8 text; use `content: attachment`")
                })?;
                builder.header(ContentType::TEXT_PLAIN).body(body)
            }
            Content::Attachment => {
                let name = render(
                    &settings.attachment_name,
                    delivery.channel.as_str(),
                    delivery.destination.as_str(),
                    delivery.message_id,
                    delivery.data_type,
                )
                .map_err(SendError::permanent)?;
                let media_type = ContentType::parse(content_type(delivery.data_type)).unwrap_or(
                    ContentType::parse("application/octet-stream")
                        .map_err(|e| SendError::permanent(format!("invalid media type: {e}")))?,
                );
                builder.multipart(
                    MultiPart::mixed()
                        .singlepart(SinglePart::plain(text(&settings.text)?))
                        .singlepart(
                            Attachment::new(name).body(delivery.payload.clone(), media_type),
                        ),
                )
            }
        };
        built.map_err(|e| SendError::permanent(format!("cannot build the email: {e}")))
    }

    fn transport(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>, SendError> {
        let settings = &self.settings;
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&settings.host)
            .port(self.port)
            .timeout(Some(settings.timeout.0))
            .tls(match (&self.tls, settings.security) {
                (Some(parameters), SmtpSecurity::Tls) => Tls::Wrapper(parameters.clone()),
                (Some(parameters), _) => Tls::Required(parameters.clone()),
                (None, _) => Tls::None,
            });
        if let Some(name) = &settings.hello_name {
            builder = builder.hello_name(ClientId::Domain(name.clone()));
        }
        if let Some(username) = &settings.username {
            let password = secret(
                settings.password.as_ref(),
                settings.password_env.as_ref(),
                "password",
            )
            .map_err(SendError::temporary)?
            .unwrap_or_default();
            builder = builder.credentials(Credentials::new(username.clone(), password));
        }
        Ok(builder.build())
    }
}

#[async_trait]
impl DestinationConnector for SmtpDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let email = self.email(delivery)?;
        let transport = self.transport()?;
        let sent = tokio::time::timeout(self.settings.timeout.0, transport.send(email))
            .await
            .map_err(|_| SendError::temporary("the SMTP server did not answer in time"))?;
        match sent {
            Ok(response) => Ok(Some(
                format!(
                    "{} {}",
                    response.code(),
                    response.message().collect::<Vec<_>>().join(" ")
                )
                .into_bytes(),
            )),
            Err(e) => {
                let code = e.status().map(|code| code.to_string());
                let message = format!("SMTP delivery to {} failed: {e}", self.settings.host);
                if e.is_permanent() && !matches!(code.as_deref(), Some("530" | "535")) {
                    Err(SendError::permanent(message))
                } else {
                    Err(SendError::temporary(message))
                }
            }
        }
    }
}

/// Registers the `smtp` destination.
pub fn register(registry: &mut Registry) {
    registry.add_destination("smtp", |config: &DestinationConfig| {
        let settings: SmtpSettings = parse(&config.settings, "smtp destination")?;
        Ok(Arc::new(SmtpDestination::new(settings)?) as Arc<dyn DestinationConnector>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(extra: &str) -> Result<SmtpDestination, EngineError> {
        let mut settings: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{"host":"mail.lab.test","from":"OXIM <oxim@lab.test>","to":["lab@lab.test"]}"#,
        )
        .unwrap();
        let extra: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&format!("{{{extra}}}")).unwrap();
        settings.extend(extra);
        SmtpDestination::new(serde_json::from_value(serde_json::Value::Object(settings)).unwrap())
    }

    #[test]
    fn checks_settings() {
        assert_eq!(destination("").unwrap().port, 587);
        assert_eq!(destination(r#""security":"tls""#).unwrap().port, 465);
        assert_eq!(destination(r#""security":"none""#).unwrap().port, 25);
        assert!(destination(r#""to":[]"#).is_err());
        assert!(destination(r#""to":["not an address"]"#).is_err());
        assert!(destination(r#""password":"x""#).is_err());
        assert!(destination(r#""subject":"{unknown}""#).is_err());
        assert!(destination(r#""security":"none","tls":{}"#).is_err());
    }
}
