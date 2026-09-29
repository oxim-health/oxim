//! SOAP 1.1 and 1.2 web services: a client that delivers messages in SOAP
//! envelopes, and an endpoint that receives them.
//!
//! Destination type `soap`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `url` | required | Service endpoint (`http://` or `https://`) |
//! | `version` | `1.1` | `1.1` or `1.2` |
//! | `action` | none | SOAP action (`SOAPAction` header in 1.1, `action` media type parameter in 1.2) |
//! | `envelope` | `wrap` | `wrap` (the message becomes the Body content) or `none` (the message already is a complete envelope) |
//! | `payload_element` | none | With `wrap`: an element such as `hl7:SubmitMessage` that holds the message as escaped text, for non-XML messages such as HL7 v2 |
//! | `payload_namespace` | none | Namespace of `payload_element`'s prefix |
//! | `headers` | none | Extra HTTP headers |
//! | `ws_security` | none | WS-Security UsernameToken: `{username, password_env, password_type: digest}` (`text` or `digest`) |
//! | `oauth2` | none | OAuth 2.0 client credentials; see [`oxim_connectors::oauth2`] |
//! | `tls` | system roots | TLS settings; see [`oxim_connectors::tls`] |
//! | `timeout` | `30s` | Limit for the whole exchange |
//! | `max_response_size` | 16 MiB | Largest accepted response |
//! | `response` | `body` | What is stored as the response: `body` (the Body content) or `envelope` |
//!
//! A SOAP fault with a `Client`/`Sender` code fails the delivery
//! permanently; `Server`/`Receiver` and other faults are retried. HTTP
//! errors without a fault are classified like the `http` destination.
//! MTOM/XOP attachments are not produced; binary content is sent as
//! escaped text inside `payload_element`.
//!
//! Source type `soap`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `listen` | required | Address to listen on |
//! | `path` | `/` | Accepted path prefix |
//! | `max_body` | 16 MiB | Largest accepted request |
//! | `max_connections` | `100` | Concurrent connections |
//! | `store` | `body` | What becomes the message: `body` (the Body content) or `envelope` |
//! | `acknowledgment` | an `oxim:Acknowledgment` element | Body content answered once the message is stored; `{message_id}` is replaced |
//! | `reply_element` | `oxim:Reply` | Element that holds a non-XML reply as escaped text |
//! | `auth` | none | `{type: basic, username, password_env}` or `{type: bearer, token_env}` |
//! | `tls` | none | TLS listener settings; see [`oxim_connectors::tls`] |
//!
//! The endpoint accepts SOAP 1.1 and 1.2 and answers in the version of the
//! request. When the channel answers requests (`source.response`) the
//! reply becomes the response Body. Malformed requests get a
//! `Client`/`Sender` fault and storage failures a `Server`/`Receiver`
//! fault, so the caller retries.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderName, HeaderValue, WWW_AUTHENTICATE};
use http::{Method, Request, Response, StatusCode, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use oxim_connectors::http::classify;
use oxim_connectors::http_listener::HttpAuth;
use oxim_connectors::oauth2::{OAuth2Client, OAuth2Settings};
use oxim_connectors::tls::{ClientTlsSettings, ServerTlsSettings, client_config, server_config};
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_model::MessageStatus;
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, warn};

use crate::util::{iso_utc, now, parse, secret, xml_escape};
use crate::xml;

const SOAP11: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const SOAP12: &str = "http://www.w3.org/2003/05/soap-envelope";
const WSSE: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
const WSU: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
const PASSWORD_TEXT: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordText";
const PASSWORD_DIGEST: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest";
const BASE64_BINARY: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary";

/// The SOAP version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub enum SoapVersion {
    /// SOAP 1.1.
    #[default]
    #[serde(rename = "1.1")]
    V11,
    /// SOAP 1.2.
    #[serde(rename = "1.2")]
    V12,
}

impl SoapVersion {
    fn namespace(self) -> &'static str {
        match self {
            Self::V11 => SOAP11,
            Self::V12 => SOAP12,
        }
    }

    fn content_type(self, action: Option<&str>) -> String {
        match (self, action) {
            (Self::V11, _) => "text/xml; charset=utf-8".to_owned(),
            (Self::V12, Some(action)) => format!(
                "application/soap+xml; charset=utf-8; action=\"{}\"",
                action.replace('"', "")
            ),
            (Self::V12, None) => "application/soap+xml; charset=utf-8".to_owned(),
        }
    }
}

/// Whether the destination builds the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeMode {
    /// The message becomes the Body content.
    #[default]
    Wrap,
    /// The message is a complete envelope.
    None,
}

/// What of a SOAP message is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoapPart {
    /// The content of the Body.
    #[default]
    Body,
    /// The whole envelope.
    Envelope,
}

/// How the UsernameToken password is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasswordType {
    /// `Base64(SHA-1(nonce + created + password))`.
    #[default]
    Digest,
    /// The password itself (use only over TLS).
    Text,
}

/// WS-Security UsernameToken settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WsSecurity {
    /// User name.
    pub username: String,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// The password inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// How the password is sent.
    #[serde(default)]
    pub password_type: PasswordType,
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

fn default_max_size() -> usize {
    16 * 1024 * 1024
}

/// Settings of the `soap` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoapDestinationSettings {
    /// Service endpoint.
    pub url: String,
    /// SOAP version.
    #[serde(default)]
    pub version: SoapVersion,
    /// SOAP action.
    #[serde(default)]
    pub action: Option<String>,
    /// Whether the envelope is built.
    #[serde(default)]
    pub envelope: EnvelopeMode,
    /// Element holding the message as escaped text.
    #[serde(default)]
    pub payload_element: Option<String>,
    /// Namespace of the payload element.
    #[serde(default)]
    pub payload_namespace: Option<String>,
    /// Extra HTTP headers.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// WS-Security UsernameToken.
    #[serde(default)]
    pub ws_security: Option<WsSecurity>,
    /// OAuth 2.0 client credentials.
    #[serde(default)]
    pub oauth2: Option<OAuth2Settings>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Limit for the exchange.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
    /// Largest accepted response.
    #[serde(default = "default_max_size")]
    pub max_response_size: usize,
    /// What is stored as the response.
    #[serde(default)]
    pub response: SoapPart,
}

/// Removes a leading XML declaration, which may not appear inside a Body.
fn without_declaration(text: &str) -> &str {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    match trimmed.strip_prefix("<?xml") {
        Some(rest) => rest
            .split_once("?>")
            .map_or(trimmed, |(_, after)| after.trim_start()),
        None => trimmed,
    }
}

/// The start tag attributes declaring the namespace of `element`'s prefix.
fn namespace_declaration(element: &str, namespace: Option<&str>) -> String {
    match (namespace, element.split_once(':')) {
        (Some(namespace), Some((prefix, _))) => {
            format!(" xmlns:{prefix}=\"{}\"", xml_escape(namespace))
        }
        (Some(namespace), None) => format!(" xmlns=\"{}\"", xml_escape(namespace)),
        (None, _) => String::new(),
    }
}

fn valid_element_name(name: &str) -> bool {
    let mut parts = name.split(':');
    let valid = |part: &str| {
        part.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && part
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    };
    let first = parts.next().is_some_and(valid);
    let second = parts.next().is_none_or(valid);
    first && second && parts.next().is_none()
}

/// A WS-Security header with a UsernameToken.
fn security_header(
    settings: &WsSecurity,
    password: &str,
    soap_prefix: &str,
    nonce: &[u8],
    created: &str,
) -> String {
    let (kind, value) = match settings.password_type {
        PasswordType::Text => (PASSWORD_TEXT, password.to_owned()),
        PasswordType::Digest => {
            let mut input = nonce.to_vec();
            input.extend_from_slice(created.as_bytes());
            input.extend_from_slice(password.as_bytes());
            let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &input);
            (
                PASSWORD_DIGEST,
                base64::engine::general_purpose::STANDARD.encode(digest.as_ref()),
            )
        }
    };
    format!(
        "<wsse:Security xmlns:wsse=\"{WSSE}\" xmlns:wsu=\"{WSU}\" {soap_prefix}:mustUnderstand=\"{must}\">\
<wsse:UsernameToken wsu:Id=\"UsernameToken-1\">\
<wsse:Username>{username}</wsse:Username>\
<wsse:Password Type=\"{kind}\">{password}</wsse:Password>\
<wsse:Nonce EncodingType=\"{BASE64_BINARY}\">{nonce}</wsse:Nonce>\
<wsu:Created>{created}</wsu:Created>\
</wsse:UsernameToken></wsse:Security>",
        must = if soap_prefix == "soap" { "1" } else { "true" },
        username = xml_escape(&settings.username),
        password = xml_escape(&value),
        nonce = base64::engine::general_purpose::STANDARD.encode(nonce),
    )
}

/// A fault read from a response.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fault {
    /// The local part of the fault code: `Client`, `Server`, `Sender`,
    /// `Receiver`, ...
    code: String,
    reason: String,
}

fn local(code: &str) -> &str {
    code.rsplit(':').next().unwrap_or(code).trim()
}

/// The fault of an envelope, if it carries one.
fn fault(envelope: &[u8]) -> Option<Fault> {
    let mut found = false;
    let (mut code, mut reason) = (String::new(), String::new());
    xml::walk(envelope, |path, text| {
        let Some(at) = path.iter().position(|name| name == "Fault") else {
            return;
        };
        if at == 0 || path[at - 1] != "Body" {
            return;
        }
        found = true;
        let inner: Vec<&str> = path[at + 1..].iter().map(String::as_str).collect();
        match inner.as_slice() {
            ["faultcode"] => code = local(text).to_owned(),
            ["faultstring"] => reason = text.trim().to_owned(),
            ["Code", "Value"] if code.is_empty() => code = local(text).to_owned(),
            ["Reason", "Text"] => reason = text.trim().to_owned(),
            _ => {}
        }
    })
    .ok()?;
    found.then_some(Fault { code, reason })
}

/// Delivers messages to a SOAP service.
pub struct SoapDestination {
    settings: SoapDestinationSettings,
    uri: Uri,
    headers: Vec<(HeaderName, HeaderValue)>,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    oauth2: Option<Arc<OAuth2Client>>,
}

impl std::fmt::Debug for SoapDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoapDestination")
            .field("uri", &self.uri)
            .field("version", &self.settings.version)
            .finish_non_exhaustive()
    }
}

impl SoapDestination {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: SoapDestinationSettings) -> Result<Self, EngineError> {
        let error = |message: String| EngineError::Config(format!("soap destination: {message}"));
        let uri: Uri = settings
            .url
            .parse()
            .map_err(|e| error(format!("invalid url {:?}: {e}", settings.url)))?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
            return Err(error(format!(
                "url {:?} must be an absolute http:// or https:// URL",
                settings.url
            )));
        }
        if settings.envelope == EnvelopeMode::None
            && (settings.ws_security.is_some() || settings.payload_element.is_some())
        {
            return Err(error(
                "ws_security and payload_element need `envelope: wrap`".into(),
            ));
        }
        if let Some(element) = &settings.payload_element
            && !valid_element_name(element)
        {
            return Err(error(format!("invalid payload_element {element:?}")));
        }
        if let Some(security) = &settings.ws_security
            && security.password.is_some() == security.password_env.is_some()
        {
            return Err(error(
                "ws_security needs exactly one of password_env and password".into(),
            ));
        }
        let headers = settings
            .headers
            .iter()
            .map(|(name, value)| {
                Ok((
                    HeaderName::from_bytes(name.as_bytes())
                        .map_err(|e| error(format!("invalid header name {name:?}: {e}")))?,
                    HeaderValue::from_str(value)
                        .map_err(|e| error(format!("invalid value for header {name}: {e}")))?,
                ))
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
        let tls = settings.tls.clone().unwrap_or_default();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(client_config(&tls)?)
            .https_or_http()
            .enable_http1()
            .build();
        let oauth2 = settings.oauth2.clone().map(OAuth2Client::new).transpose()?;
        Ok(Self {
            settings,
            uri,
            headers,
            client: Client::builder(TokioExecutor::new()).build(connector),
            oauth2,
        })
    }

    /// The request body.
    fn envelope(&self, payload: &[u8]) -> Result<Vec<u8>, SendError> {
        let settings = &self.settings;
        if settings.envelope == EnvelopeMode::None {
            return Ok(payload.to_vec());
        }
        let text = std::str::from_utf8(payload)
            .map_err(|_| SendError::permanent("the message is not UTF-8 text"))?;
        let content = match &settings.payload_element {
            Some(element) => format!(
                "<{element}{}>{}</{element}>",
                namespace_declaration(element, settings.payload_namespace.as_deref()),
                xml_escape(text)
            ),
            None => without_declaration(text).to_owned(),
        };
        let header = match &settings.ws_security {
            Some(security) => {
                let password = secret(
                    security.password.as_ref(),
                    security.password_env.as_ref(),
                    "WS-Security password",
                )
                .map_err(SendError::temporary)?
                .unwrap_or_default();
                let nonce: [u8; 16] = std::array::from_fn(|_| fastrand::u8(..));
                format!(
                    "<soap:Header>{}</soap:Header>",
                    security_header(security, &password, "soap", &nonce, &iso_utc(now()))
                )
            }
            None => String::new(),
        };
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><soap:Envelope xmlns:soap=\"{}\">{header}<soap:Body>{content}</soap:Body></soap:Envelope>",
            settings.version.namespace()
        )
        .into_bytes())
    }

    async fn exchange(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let settings = &self.settings;
        let body = self.envelope(&delivery.payload)?;
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(self.uri.clone())
            .header(
                CONTENT_TYPE,
                settings.version.content_type(settings.action.as_deref()),
            )
            .header("X-OXIM-Message-Id", delivery.message_id.to_string());
        if settings.version == SoapVersion::V11 {
            builder = builder.header(
                "SOAPAction",
                format!(
                    "\"{}\"",
                    settings
                        .action
                        .as_deref()
                        .unwrap_or_default()
                        .replace('"', "")
                ),
            );
        }
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        if let Some(oauth2) = &self.oauth2 {
            builder = builder.header(http::header::AUTHORIZATION, oauth2.authorization().await?);
        }
        let request = builder
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| SendError::permanent(format!("cannot build the request: {e}")))?;
        let response =
            self.client.request(request).await.map_err(|e| {
                SendError::temporary(format!("request to {} failed: {e}", self.uri))
            })?;
        let status = response.status();
        let answer = Limited::new(response.into_body(), settings.max_response_size)
            .collect()
            .await
            .map_err(|e| SendError::temporary(format!("reading the response failed: {e}")))?
            .to_bytes();
        if let Some(fault) = fault(&answer) {
            let message = format!(
                "SOAP fault {} from {}: {}",
                fault.code, self.uri, fault.reason
            );
            return if matches!(fault.code.as_str(), "Client" | "Sender") {
                Err(SendError::permanent(message))
            } else {
                Err(SendError::temporary(message))
            };
        }
        if status == StatusCode::UNAUTHORIZED
            && let Some(oauth2) = &self.oauth2
        {
            oauth2.invalidate().await;
            return Err(SendError::temporary(format!(
                "{} rejected the OAuth 2.0 token",
                self.uri
            )));
        }
        let stored = classify(status, &answer)?;
        Ok(match settings.response {
            SoapPart::Envelope => stored,
            SoapPart::Body => match xml::content_range(&answer, &["Envelope", "Body"]) {
                Ok(Some((from, to))) => Some(answer[from..to].trim_ascii().to_vec()),
                _ => stored,
            },
        })
    }
}

#[async_trait]
impl DestinationConnector for SoapDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        tokio::time::timeout(self.settings.timeout.0, self.exchange(delivery))
            .await
            .map_err(|_| {
                SendError::temporary(format!("no response from {} within the timeout", self.uri))
            })?
    }
}

fn default_path() -> String {
    "/".to_owned()
}

fn default_max_connections() -> usize {
    100
}

fn default_acknowledgment() -> String {
    "<oxim:Acknowledgment xmlns:oxim=\"urn:oxim:soap\"><oxim:MessageId>{message_id}</oxim:MessageId></oxim:Acknowledgment>".to_owned()
}

fn default_reply_element() -> String {
    "oxim:Reply".to_owned()
}

/// Settings of the `soap` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoapSourceSettings {
    /// Address to listen on.
    pub listen: String,
    /// Accepted path prefix.
    #[serde(default = "default_path")]
    pub path: String,
    /// Largest accepted request.
    #[serde(default = "default_max_size")]
    pub max_body: usize,
    /// Concurrent connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// What becomes the message.
    #[serde(default)]
    pub store: SoapPart,
    /// Body content answered once a message is stored.
    #[serde(default = "default_acknowledgment")]
    pub acknowledgment: String,
    /// Element holding a non-XML reply.
    #[serde(default = "default_reply_element")]
    pub reply_element: String,
    /// Client authentication.
    #[serde(default)]
    pub auth: Option<HttpAuth>,
    /// TLS.
    #[serde(default)]
    pub tls: Option<ServerTlsSettings>,
}

/// Receives SOAP requests.
pub struct SoapSource {
    settings: Arc<SoapSourceSettings>,
    tls: Option<(TlsAcceptor, Duration)>,
}

impl std::fmt::Debug for SoapSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoapSource")
            .field("listen", &self.settings.listen)
            .finish_non_exhaustive()
    }
}

impl SoapSource {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: SoapSourceSettings) -> Result<Self, EngineError> {
        let error = |message: String| EngineError::Config(format!("soap source: {message}"));
        if !settings.path.starts_with('/') {
            return Err(error(format!(
                "path must start with '/', not {:?}",
                settings.path
            )));
        }
        if !valid_element_name(&settings.reply_element) {
            return Err(error(format!(
                "invalid reply_element {:?}",
                settings.reply_element
            )));
        }
        let tls = settings
            .tls
            .as_ref()
            .map(|tls| {
                Ok::<_, EngineError>((
                    TlsAcceptor::from(Arc::new(server_config(tls)?)),
                    tls.handshake_timeout.0,
                ))
            })
            .transpose()?;
        Ok(Self {
            settings: Arc::new(settings),
            tls,
        })
    }
}

/// The expected `Authorization` value and the challenge.
fn credentials(auth: &HttpAuth) -> Result<(Vec<u8>, &'static str), String> {
    match auth {
        HttpAuth::Basic {
            username,
            password,
            password_env,
        } => {
            let password = secret(password.as_ref(), password_env.as_ref(), "password")?
                .ok_or("set password_env (or password)")?;
            Ok((
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{username}:{password}"))
                )
                .into_bytes(),
                "Basic realm=\"oxim\"",
            ))
        }
        HttpAuth::Bearer { token, token_env } => {
            let token = secret(token.as_ref(), token_env.as_ref(), "token")?
                .ok_or("set token_env (or token)")?;
            Ok((format!("Bearer {token}").into_bytes(), "Bearer"))
        }
    }
}

/// Compares without an early exit.
fn same(a: &[u8], b: &[u8]) -> bool {
    let mut difference = u8::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        difference |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0xff);
    }
    difference == 0
}

struct Endpoint {
    context: SourceContext,
    settings: Arc<SoapSourceSettings>,
    credentials: Option<(Vec<u8>, &'static str)>,
}

fn envelope_response(
    version: SoapVersion,
    status: StatusCode,
    body: &str,
) -> Response<Full<Bytes>> {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><soap:Envelope xmlns:soap=\"{}\"><soap:Body>{body}</soap:Body></soap:Envelope>",
        version.namespace()
    );
    let mut response = Response::new(Full::new(Bytes::from(xml)));
    *response.status_mut() = status;
    if let Ok(value) = HeaderValue::from_str(&version.content_type(None)) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

fn fault_response(version: SoapVersion, client: bool, reason: &str) -> Response<Full<Bytes>> {
    let reason = xml_escape(reason);
    match version {
        SoapVersion::V11 => envelope_response(
            version,
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!(
                "<soap:Fault><faultcode>soap:{}</faultcode><faultstring>{reason}</faultstring></soap:Fault>",
                if client { "Client" } else { "Server" }
            ),
        ),
        SoapVersion::V12 => envelope_response(
            version,
            if client {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            &format!(
                "<soap:Fault><soap:Code><soap:Value>soap:{}</soap:Value></soap:Code><soap:Reason><soap:Text xml:lang=\"en\">{reason}</soap:Text></soap:Reason></soap:Fault>",
                if client { "Sender" } else { "Receiver" }
            ),
        ),
    }
}

fn plain(status: StatusCode, text: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(text.to_owned())));
    *response.status_mut() = status;
    response
}

impl Endpoint {
    async fn handle(&self, request: Request<Incoming>, peer: SocketAddr) -> Response<Full<Bytes>> {
        let settings = &self.settings;
        if !request.uri().path().starts_with(&settings.path) {
            return plain(StatusCode::NOT_FOUND, "no such path");
        }
        if request.method() != Method::POST {
            let mut response = plain(StatusCode::METHOD_NOT_ALLOWED, "use POST");
            response
                .headers_mut()
                .insert(http::header::ALLOW, HeaderValue::from_static("POST"));
            return response;
        }
        if let Some((expected, challenge)) = &self.credentials {
            let given = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .map(HeaderValue::as_bytes)
                .unwrap_or_default();
            if !same(given, expected) {
                let mut response = plain(StatusCode::UNAUTHORIZED, "authentication required");
                response
                    .headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
                return response;
            }
        }
        let action = request
            .headers()
            .get("soapaction")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"').to_owned());
        let body = match Limited::new(request.into_body(), settings.max_body)
            .collect()
            .await
        {
            Ok(body) => body.to_bytes(),
            Err(_) => return plain(StatusCode::PAYLOAD_TOO_LARGE, "the request is too large"),
        };
        let version = match xml::root_namespace(&body).as_deref() {
            Some(SOAP12) => SoapVersion::V12,
            Some(SOAP11) => SoapVersion::V11,
            _ => {
                return fault_response(
                    SoapVersion::V11,
                    true,
                    "the request is not a SOAP 1.1 or 1.2 envelope",
                );
            }
        };
        let message = match settings.store {
            SoapPart::Envelope => body.to_vec(),
            SoapPart::Body => match xml::content_range(&body, &["Envelope", "Body"]) {
                Ok(Some((from, to))) => body[from..to].trim_ascii().to_vec(),
                Ok(None) => return fault_response(version, true, "the envelope has no Body"),
                Err(e) => return fault_response(version, true, &e),
            },
        };
        let mut info = SubmitInfo {
            peer: Some(peer.to_string()),
            ..SubmitInfo::default()
        };
        info.metadata.insert(
            "soap.version".to_owned(),
            match version {
                SoapVersion::V11 => "1.1",
                SoapVersion::V12 => "1.2",
            }
            .to_owned(),
        );
        if let Some(action) = action.filter(|a| !a.is_empty()) {
            info.metadata.insert("soap.action".to_owned(), action);
        }
        let context = &self.context;
        if context.responds() {
            return match context.request(message, info).await {
                Ok(reply) => match reply.data {
                    Some(data) => {
                        let text = String::from_utf8_lossy(&data);
                        let content = if text.trim_start().starts_with('<') {
                            without_declaration(&text).to_owned()
                        } else {
                            let element = &settings.reply_element;
                            format!(
                                "<{element}{}>{}</{element}>",
                                namespace_declaration(element, Some("urn:oxim:soap")),
                                xml_escape(&text)
                            )
                        };
                        envelope_response(version, StatusCode::OK, &content)
                    }
                    None if reply.status == MessageStatus::Error => fault_response(
                        version,
                        false,
                        reply.error.as_deref().unwrap_or("processing failed"),
                    ),
                    None => envelope_response(
                        version,
                        StatusCode::OK,
                        &settings
                            .acknowledgment
                            .replace("{message_id}", &reply.message_id.to_string()),
                    ),
                },
                Err(e) => {
                    warn!(channel = %context.channel(), %peer, error = %e, "SOAP request could not be stored");
                    fault_response(version, false, "the message could not be stored")
                }
            };
        }
        match context.submit(message, info).await {
            Ok(id) => envelope_response(
                version,
                StatusCode::OK,
                &settings
                    .acknowledgment
                    .replace("{message_id}", &id.to_string()),
            ),
            Err(e) => {
                warn!(channel = %context.channel(), %peer, error = %e, "SOAP request could not be stored");
                fault_response(version, false, "the message could not be stored")
            }
        }
    }
}

#[async_trait]
impl SourceConnector for SoapSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let credentials = self
            .settings
            .auth
            .as_ref()
            .map(credentials)
            .transpose()
            .map_err(|e| ConnectorError(format!("soap source authentication: {e}")))?;
        let listener = TcpListener::bind(&self.settings.listen)
            .await
            .map_err(|e| {
                ConnectorError(format!("cannot listen on {}: {e}", self.settings.listen))
            })?;
        let endpoint = Arc::new(Endpoint {
            context: context.clone(),
            settings: self.settings.clone(),
            credentials,
        });
        let permits = Arc::new(Semaphore::new(self.settings.max_connections.max(1)));
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (socket, peer) = tokio::select! {
                () = context.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        warn!(channel = %context.channel(), error = %e, "accept failed");
                        continue;
                    }
                },
                Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                warn!(channel = %context.channel(), %peer, "connection limit reached; closing connection");
                continue;
            };
            let endpoint = endpoint.clone();
            let tls = self.tls.clone();
            connections.spawn(async move {
                let service = {
                    let endpoint = endpoint.clone();
                    service_fn(move |request| {
                        let endpoint = endpoint.clone();
                        async move { Ok::<_, Infallible>(endpoint.handle(request, peer).await) }
                    })
                };
                let builder = {
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(30));
                    builder
                };
                let cancelled = endpoint.context.clone();
                match tls {
                    Some((acceptor, limit)) => {
                        let Ok(Ok(stream)) =
                            tokio::time::timeout(limit, acceptor.accept(socket)).await
                        else {
                            debug!(%peer, "TLS handshake failed");
                            return;
                        };
                        let connection = builder.serve_connection(TokioIo::new(stream), service);
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = connection.as_mut() => {}
                            () = cancelled.cancelled() => {
                                connection.as_mut().graceful_shutdown();
                                let _ = connection.await;
                            }
                        }
                    }
                    None => {
                        let connection = builder.serve_connection(TokioIo::new(socket), service);
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = connection.as_mut() => {}
                            () = cancelled.cancelled() => {
                                connection.as_mut().graceful_shutdown();
                                let _ = connection.await;
                            }
                        }
                    }
                }
                drop(permit);
            });
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        connections.shutdown().await;
        Ok(())
    }
}

/// Registers the `soap` source and destination.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("soap", |config: &SourceConfig| {
            let settings: SoapSourceSettings = parse(&config.settings, "soap source")?;
            Ok(Arc::new(SoapSource::new(settings)?) as Arc<dyn SourceConnector>)
        })
        .add_destination("soap", |config: &DestinationConfig| {
            let settings: SoapDestinationSettings = parse(&config.settings, "soap destination")?;
            Ok(Arc::new(SoapDestination::new(settings)?) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_password_digests() {
        // Digest = Base64(SHA-1(nonce + created + password)), checked with an
        // independent computation of the same formula.
        let nonce = b"0123456789abcdef";
        let created = "2026-09-29T12:00:00Z";
        let security = WsSecurity {
            username: "lab".into(),
            password_env: None,
            password: Some("secret".into()),
            password_type: PasswordType::Digest,
        };
        let header = security_header(&security, "secret", "soap", nonce, created);
        let mut input = nonce.to_vec();
        input.extend_from_slice(created.as_bytes());
        input.extend_from_slice(b"secret");
        let expected = base64::engine::general_purpose::STANDARD
            .encode(ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &input).as_ref());
        assert!(
            header.contains(&format!("#PasswordDigest\">{expected}</wsse:Password>")),
            "{header}"
        );
        assert!(header.contains("<wsse:Nonce EncodingType="));
        assert!(header.contains("<wsu:Created>2026-09-29T12:00:00Z</wsu:Created>"));
        assert!(header.contains("soap:mustUnderstand=\"1\""));
    }

    #[test]
    fn reads_faults() {
        let v11 = br#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>Invalid patient id</faultstring></s:Fault></s:Body></s:Envelope>"#;
        assert_eq!(
            fault(v11),
            Some(Fault {
                code: "Client".into(),
                reason: "Invalid patient id".into()
            })
        );
        let v12 = br#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"><env:Body><env:Fault><env:Code><env:Value>env:Receiver</env:Value><env:Subcode><env:Value>m:Busy</env:Value></env:Subcode></env:Code><env:Reason><env:Text xml:lang="en">Try later</env:Text></env:Reason></env:Fault></env:Body></env:Envelope>"#;
        assert_eq!(
            fault(v12),
            Some(Fault {
                code: "Receiver".into(),
                reason: "Try later".into()
            })
        );
        let ok = br#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><r>Fault</r></s:Body></s:Envelope>"#;
        assert_eq!(fault(ok), None);
    }

    #[test]
    fn strips_declarations_and_checks_names() {
        assert_eq!(without_declaration("<?xml version=\"1.0\"?>\n<a/>"), "<a/>");
        assert_eq!(without_declaration("<a/>"), "<a/>");
        assert!(valid_element_name("hl7:SubmitMessage"));
        assert!(valid_element_name("Message"));
        assert!(!valid_element_name("a b"));
        assert!(!valid_element_name("a:b:c"));
        assert!(!valid_element_name("1a"));
    }
}
