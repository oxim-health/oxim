//! The `dicomweb-stow` destination: stores each object on a DICOMweb
//! server with STOW-RS (PS3.18 section 10.5).
//!
//! Every delivery is one `multipart/related; type="application/dicom"`
//! POST to `{url}/studies` containing the Part 10 object.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue};
use http::{Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use oxim_core::config::DurationText;
use oxim_core::{DestinationConfig, DestinationConnector, EngineError, SendError, async_trait};
use oxim_store::Delivery;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use serde::Deserialize;
use tracing::debug;

use crate::net::settings;
use crate::scu::failure_is_temporary;

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(120))
}

fn default_max_response_size() -> usize {
    16 * 1024 * 1024
}

/// Settings of the `dicomweb-stow` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StowSettings {
    /// Base URL of the DICOMweb service, for example
    /// `https://pacs.example/dicom-web`.
    pub url: String,
    /// Bearer token sent as `Authorization: Bearer ...`.
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// Environment variable holding the bearer token, read when the channel
    /// is deployed.
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    /// Extra request headers.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Limit for the whole request, including the response body.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
    /// PEM file with extra trusted certificate authorities.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    /// Largest accepted response body.
    #[serde(default = "default_max_response_size")]
    pub max_response_size: usize,
}

/// A STOW-RS destination.
pub struct StowDestination {
    uri: Uri,
    headers: Vec<(HeaderName, HeaderValue)>,
    timeout: Duration,
    max_response_size: usize,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl std::fmt::Debug for StowDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StowDestination")
            .field("uri", &self.uri)
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

/// The failure described by a STOW-RS response body: the Failure Reason
/// (0008,1197) of the first item of the Failed SOP Sequence (0008,1198).
fn failed_instance(body: &[u8]) -> Option<Option<u16>> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let failed = json.get("00081198")?.get("Value")?.as_array()?;
    let first = failed.first()?;
    let reason = first
        .get("00081197")
        .and_then(|reason| reason.get("Value"))
        .and_then(|value| value.get(0))
        .and_then(serde_json::Value::as_u64)
        .and_then(|reason| u16::try_from(reason).ok());
    Some(reason)
}

/// How a STOW-RS response answers a delivery.
///
/// `200` stores the instance. `202` stores it with warnings unless the
/// response lists it as failed. A failed instance, `409` and other `4xx`
/// responses fail the delivery, except failure reasons that may succeed
/// later (`A7xx`, `0110`, `0213`); `408`, `429`, `3xx` and `5xx` are
/// retried.
pub(crate) fn classify(status: StatusCode, body: &[u8]) -> Result<Option<Vec<u8>>, SendError> {
    let detail = || String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
    let failure = failed_instance(body);
    if status.is_success() {
        let Some(reason) = failure else {
            return Ok(Some(body.to_vec()));
        };
        return Err(instance_failed(status, reason));
    }
    if let Some(reason) = failure {
        return Err(instance_failed(status, reason));
    }
    let message = format!("HTTP {status}: {}", detail());
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

fn instance_failed(status: StatusCode, reason: Option<u16>) -> SendError {
    match reason {
        Some(reason) => {
            let message = format!(
                "HTTP {status}: the server did not store the instance (failure reason {reason:04X})"
            );
            if failure_is_temporary(reason) {
                SendError::temporary(message)
            } else {
                SendError::permanent(message)
            }
        }
        None => SendError::permanent(format!(
            "HTTP {status}: the server did not store the instance"
        )),
    }
}

/// A multipart boundary that does not occur in `payload`.
fn boundary(payload: &[u8], seed: &str) -> String {
    let mut n = 0u32;
    loop {
        let candidate = format!("oxim-{seed}-{n}");
        if memchr::memmem::find(payload, candidate.as_bytes()).is_none() {
            return candidate;
        }
        n += 1;
    }
}

/// The STOW-RS request body and its boundary.
pub(crate) fn multipart(payload: &[u8], seed: &str) -> (String, Vec<u8>) {
    let boundary = boundary(payload, seed);
    let mut body = Vec::with_capacity(payload.len() + 2 * boundary.len() + 64);
    body.extend_from_slice(b"--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"\r\nContent-Type: application/dicom\r\n\r\n");
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\r\n--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"--\r\n");
    (boundary, body)
}

impl StowDestination {
    /// Validates the settings and creates the destination.
    pub fn new(settings: StowSettings) -> Result<Self, EngineError> {
        let config =
            |message: String| EngineError::Config(format!("dicomweb-stow destination: {message}"));
        let url = format!("{}/studies", settings.url.trim().trim_end_matches('/'));
        let uri: Uri = url
            .parse()
            .map_err(|e| config(format!("invalid url {:?}: {e}", settings.url)))?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
            return Err(config(format!(
                "url {:?} must be an absolute http:// or https:// URL",
                settings.url
            )));
        }
        let mut headers = settings
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
        let token = match (&settings.bearer_token, &settings.bearer_token_env) {
            (Some(_), Some(_)) => {
                return Err(config(
                    "set bearer_token or bearer_token_env, not both".into(),
                ));
            }
            (Some(token), None) => Some(token.clone()),
            (None, Some(variable)) => Some(
                std::env::var(variable)
                    .map_err(|_| config(format!("environment variable {variable} is not set")))?,
            ),
            (None, None) => None,
        };
        if let Some(token) = token {
            let mut value = HeaderValue::from_str(&format!("Bearer {}", token.trim()))
                .map_err(|_| config("the bearer token is not a valid header value".into()))?;
            value.set_sensitive(true);
            headers.push((AUTHORIZATION, value));
        }
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config(settings.ca_file.as_ref())?)
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(connector);
        Ok(Self {
            uri,
            headers,
            timeout: settings.timeout.0,
            max_response_size: settings.max_response_size,
            client,
        })
    }

    async fn request(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let (boundary, body) = multipart(&delivery.payload, &delivery.message_id.to_string());
        let content_type = HeaderValue::from_str(&format!(
            "multipart/related; type=\"application/dicom\"; boundary={boundary}"
        ))
        .map_err(|e| SendError::permanent(format!("cannot build the request: {e}")))?;
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(self.uri.clone())
            .header(CONTENT_TYPE, content_type)
            .header(ACCEPT, HeaderValue::from_static("application/dicom+json"))
            .header("X-OXIM-Message-Id", delivery.message_id.to_string());
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(Full::new(Bytes::from(body)))
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
impl DestinationConnector for StowDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        tokio::time::timeout(self.timeout, self.request(delivery))
            .await
            .map_err(|_| {
                SendError::temporary(format!("no response from {} within the timeout", self.uri))
            })?
    }
}

/// Registers the `dicomweb-stow` destination.
pub(crate) fn register(registry: &mut oxim_core::Registry) {
    registry.add_destination("dicomweb-stow", |config: &DestinationConfig| {
        let settings: StowSettings = settings(&config.settings, "dicomweb-stow destination")?;
        Ok(Arc::new(StowDestination::new(settings)?) as Arc<dyn DestinationConnector>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_responses() {
        assert!(classify(StatusCode::OK, b"{}").is_ok());
        assert!(
            classify(
                StatusCode::ACCEPTED,
                br#"{"00081199":{"vr":"SQ","Value":[{}]}}"#
            )
            .is_ok()
        );
        let failed =
            br#"{"00081198":{"vr":"SQ","Value":[{"00081197":{"vr":"US","Value":[43264]}}]}}"#;
        let error = classify(StatusCode::ACCEPTED, failed).unwrap_err();
        assert!(error.permanent);
        assert!(error.message.contains("A900"), "{}", error.message);
        let busy =
            br#"{"00081198":{"vr":"SQ","Value":[{"00081197":{"vr":"US","Value":[42752]}}]}}"#;
        assert!(!classify(StatusCode::CONFLICT, busy).unwrap_err().permanent);
        assert!(classify(StatusCode::CONFLICT, b"").unwrap_err().permanent);
        assert!(
            classify(StatusCode::BAD_REQUEST, b"")
                .unwrap_err()
                .permanent
        );
        assert!(
            !classify(StatusCode::SERVICE_UNAVAILABLE, b"")
                .unwrap_err()
                .permanent
        );
        assert!(
            !classify(StatusCode::TOO_MANY_REQUESTS, b"")
                .unwrap_err()
                .permanent
        );
    }

    #[test]
    fn builds_multipart_bodies() {
        let payload = b"DICM oxim-1-0 inside";
        let (boundary, body) = multipart(payload, "1");
        assert_eq!(boundary, "oxim-1-1");
        assert!(body.starts_with(b"--oxim-1-1\r\nContent-Type: application/dicom\r\n\r\nDICM"));
        assert!(body.ends_with(b"\r\n--oxim-1-1--\r\n"));
    }

    #[test]
    fn validates_settings() {
        let base = |url: &str| StowSettings {
            url: url.to_owned(),
            bearer_token: None,
            bearer_token_env: None,
            headers: BTreeMap::new(),
            timeout: default_timeout(),
            ca_file: None,
            max_response_size: default_max_response_size(),
        };
        let stow = StowDestination::new(base("https://pacs.example/dicom-web/")).unwrap();
        assert_eq!(
            stow.uri.to_string(),
            "https://pacs.example/dicom-web/studies"
        );
        assert!(StowDestination::new(base("ftp://pacs.example/")).is_err());
        let mut both = base("http://pacs.example");
        both.bearer_token = Some("a".into());
        both.bearer_token_env = Some("B".into());
        assert!(StowDestination::new(both).is_err());
        let mut missing = base("http://pacs.example");
        missing.bearer_token_env = Some("OXIM_TEST_UNSET_VARIABLE_4711".into());
        assert!(StowDestination::new(missing).is_err());
        let mut token = base("http://pacs.example");
        token.bearer_token = Some("secret".into());
        let stow = StowDestination::new(token).unwrap();
        assert!(
            stow.headers
                .iter()
                .any(|(name, value)| name == AUTHORIZATION && value.is_sensitive())
        );
    }
}
