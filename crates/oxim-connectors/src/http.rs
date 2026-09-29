//! Sending messages to HTTP and HTTPS endpoints (REST APIs, webhooks, FHIR
//! servers).
//!
//! Destination type `http`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `url` | required | `http://` or `https://` endpoint |
//! | `method` | `POST` | `POST` or `PUT` |
//! | `headers` | none | Extra request headers, for example an `Authorization` token |
//! | `content_type` | from the data type | `Content-Type` of the request |
//! | `timeout` | `30s` | Limit for the whole request, including the response body |
//! | `ca_file` | none | PEM file with extra trusted certificate authorities, for internal CAs |
//! | `max_response_size` | 16 MiB | Largest accepted response body |
//!
//! HTTPS uses rustls with the operating system's trusted certificates plus
//! `ca_file`. A `2xx` response delivers the message and its body is stored
//! as the response. `408`, `429`, `5xx` and transport errors are retried
//! according to the destination's retry policy; other `4xx` responses fail
//! the delivery because repeating the same request cannot succeed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use http::{Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use oxim_core::config::DurationText;
use oxim_core::{
    DestinationConfig, DestinationConnector, EngineError, Registry, SendError, async_trait,
};
use oxim_model::DataType;
use oxim_store::Delivery;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use serde::Deserialize;
use tracing::debug;

use crate::net::{DEFAULT_MAX_MESSAGE, settings};

fn default_method() -> String {
    "POST".to_owned()
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

fn default_max_response_size() -> usize {
    DEFAULT_MAX_MESSAGE
}

/// Settings of the `http` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpDestinationSettings {
    /// Endpoint URL.
    pub url: String,
    /// `POST` or `PUT`.
    #[serde(default = "default_method")]
    pub method: String,
    /// Extra request headers.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// `Content-Type` of the request.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Limit for the whole request.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
    /// Extra trusted certificate authorities.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    /// Largest accepted response body.
    #[serde(default = "default_max_response_size")]
    pub max_response_size: usize,
}

/// The usual media type of a data type.
pub(crate) fn content_type(data_type: Option<DataType>) -> &'static str {
    match data_type {
        Some(DataType::Hl7V2) => "x-application/hl7-v2+er7",
        Some(DataType::Fhir) => "application/fhir+json",
        Some(DataType::Json) => "application/json",
        Some(DataType::Xml | DataType::Poct1a | DataType::Cda) => "application/xml",
        Some(DataType::Delimited) => "text/csv",
        Some(DataType::Astm | DataType::FixedWidth | DataType::Ncpdp | DataType::X12) => {
            "text/plain"
        }
        Some(DataType::Dicom) => "application/dicom",
        Some(DataType::Raw) | None => "application/octet-stream",
    }
}

/// How an HTTP status answers a delivery.
pub(crate) fn classify(status: StatusCode, body: &[u8]) -> Result<Option<Vec<u8>>, SendError> {
    if status.is_success() {
        return Ok(Some(body.to_vec()));
    }
    let detail = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
    let message = format!("HTTP {status}: {detail}");
    let retry = status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || status.is_redirection();
    if retry {
        Err(SendError::temporary(message))
    } else {
        Err(SendError::permanent(message))
    }
}

/// Sends each delivery as an HTTP request.
pub struct HttpDestination {
    uri: Uri,
    method: Method,
    headers: Vec<(HeaderName, HeaderValue)>,
    content_type: Option<HeaderValue>,
    timeout: Duration,
    max_response_size: usize,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl std::fmt::Debug for HttpDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpDestination")
            .field("uri", &self.uri)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

fn tls_config(ca_file: Option<&PathBuf>) -> Result<rustls::ClientConfig, EngineError> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for error in &native.errors {
        debug!(%error, "cannot load a system certificate");
    }
    let (_added, _ignored) = roots.add_parsable_certificates(native.certs);
    if let Some(path) = ca_file {
        let certificates = CertificateDer::pem_file_iter(path)
            .map_err(|e| EngineError::Config(format!("cannot read {}: {e}", path.display())))?;
        for certificate in certificates {
            let certificate = certificate.map_err(|e| {
                EngineError::Config(format!("invalid certificate in {}: {e}", path.display()))
            })?;
            roots.add(certificate).map_err(|e| {
                EngineError::Config(format!("invalid certificate in {}: {e}", path.display()))
            })?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| EngineError::Config(format!("TLS configuration: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

impl HttpDestination {
    /// Validates the settings and creates the destination.
    pub fn new(settings: HttpDestinationSettings) -> Result<Self, EngineError> {
        let config = |message: String| EngineError::Config(format!("http destination: {message}"));
        let uri: Uri = settings
            .url
            .parse()
            .map_err(|e| config(format!("invalid url {:?}: {e}", settings.url)))?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
            return Err(config(format!(
                "url {:?} must be an absolute http:// or https:// URL",
                settings.url
            )));
        }
        let method = match settings.method.to_ascii_uppercase().as_str() {
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            other => return Err(config(format!("method must be POST or PUT, not {other}"))),
        };
        let headers = settings
            .headers
            .iter()
            .map(|(name, value)| {
                Ok((
                    HeaderName::from_bytes(name.as_bytes())
                        .map_err(|e| config(format!("invalid header name {name:?}: {e}")))?,
                    HeaderValue::from_str(value)
                        .map_err(|e| config(format!("invalid value for header {name}: {e}")))?,
                ))
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
        let content_type = settings
            .content_type
            .as_deref()
            .map(|value| {
                HeaderValue::from_str(value)
                    .map_err(|e| config(format!("invalid content_type: {e}")))
            })
            .transpose()?;
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config(settings.ca_file.as_ref())?)
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(connector);
        Ok(Self {
            uri,
            method,
            headers,
            content_type,
            timeout: settings.timeout.0,
            max_response_size: settings.max_response_size,
            client,
        })
    }

    async fn request(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let content_type = match &self.content_type {
            Some(value) => value.clone(),
            None => HeaderValue::from_static(content_type(delivery.data_type)),
        };
        let mut builder = Request::builder()
            .method(self.method.clone())
            .uri(self.uri.clone())
            .header(CONTENT_TYPE, content_type)
            .header("X-OXIM-Message-Id", delivery.message_id.to_string());
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(Full::new(Bytes::from(delivery.payload.clone())))
            .map_err(|e| SendError::permanent(format!("cannot build the request: {e}")))?;
        let response =
            self.client.request(request).await.map_err(|e| {
                SendError::temporary(format!("request to {} failed: {e}", self.uri))
            })?;
        let status = response.status();
        let body = Limited::new(response.into_body(), self.max_response_size)
            .collect()
            .await
            .map_err(|e| SendError::temporary(format!("reading the response failed: {e}")))?
            .to_bytes();
        classify(status, &body)
    }
}

#[async_trait]
impl DestinationConnector for HttpDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        tokio::time::timeout(self.timeout, self.request(delivery))
            .await
            .map_err(|_| {
                SendError::temporary(format!("no response from {} within the timeout", self.uri))
            })?
    }
}

/// Registers the `http` destination.
pub fn register(registry: &mut Registry) {
    registry.add_destination("http", |config: &DestinationConfig| {
        let settings: HttpDestinationSettings = settings(&config.settings, "http destination")?;
        Ok(Arc::new(HttpDestination::new(settings)?) as Arc<dyn DestinationConnector>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_statuses() {
        assert_eq!(
            classify(StatusCode::OK, b"ok").unwrap(),
            Some(b"ok".to_vec())
        );
        assert_eq!(
            classify(StatusCode::CREATED, b"").unwrap(),
            Some(Vec::new())
        );
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(!classify(status, b"").unwrap_err().permanent, "{status}");
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert!(classify(status, b"").unwrap_err().permanent, "{status}");
        }
    }

    #[test]
    fn validates_settings() {
        let base = |url: &str| HttpDestinationSettings {
            url: url.to_owned(),
            method: default_method(),
            headers: BTreeMap::new(),
            content_type: None,
            timeout: default_timeout(),
            ca_file: None,
            max_response_size: default_max_response_size(),
        };
        assert!(HttpDestination::new(base("https://lis.example/api/results")).is_ok());
        assert!(HttpDestination::new(base("ftp://lis.example/")).is_err());
        assert!(HttpDestination::new(base("/relative")).is_err());
        let mut bad_method = base("http://lis.example/");
        bad_method.method = "DELETE".into();
        assert!(HttpDestination::new(bad_method).is_err());
        let mut bad_header = base("http://lis.example/");
        bad_header.headers.insert("bad header".into(), "x".into());
        assert!(HttpDestination::new(bad_header).is_err());
        assert_eq!(
            content_type(Some(DataType::Hl7V2)),
            "x-application/hl7-v2+er7"
        );
    }
}
