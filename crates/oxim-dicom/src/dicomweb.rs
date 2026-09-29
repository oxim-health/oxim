//! DICOMweb client destinations: `dicomweb-qido` (QIDO-RS search) and
//! `dicomweb-wado` (WADO-RS retrieve), PS3.18 sections 10.4 and 10.6.
//!
//! The payload of each delivery is an identifier (see [`crate::query`]):
//! QIDO-RS sends its keys as query parameters, WADO-RS retrieves the
//! study, series or instance its UIDs name.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::header::Header;
use dicom_dictionary_std::{StandardDataDictionary, tags};
use http::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use http::{Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full, Limited};
use oxim_core::config::DurationText;
use oxim_core::{DestinationConfig, DestinationConnector, EngineError, SendError, async_trait};
use oxim_store::Delivery;
use serde::Deserialize;
use tracing::debug;

use crate::environment::DicomEnvironment;
use crate::net::settings;
use crate::object::value_text;
use crate::part10;
use crate::query::{parse_identifier, text};
use crate::web::{self, HttpClient};

fn default_qido_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

fn default_wado_timeout() -> DurationText {
    DurationText(Duration::from_secs(600))
}

fn default_qido_max_response_size() -> usize {
    16 * 1024 * 1024
}

fn default_wado_max_response_size() -> usize {
    1024 * 1024 * 1024
}

/// The resource a QIDO-RS search returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QidoLevel {
    /// Studies.
    #[default]
    Studies,
    /// Series.
    Series,
    /// Instances.
    Instances,
}

impl QidoLevel {
    fn path(self) -> &'static str {
        match self {
            Self::Studies => "studies",
            Self::Series => "series",
            Self::Instances => "instances",
        }
    }
}

/// Settings of the `dicomweb-qido` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QidoSettings {
    /// Base URL of the DICOMweb service.
    pub url: String,
    /// What to search.
    #[serde(default)]
    pub level: QidoLevel,
    /// Query parameters added to every search, such as `limit`.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Bearer token sent as `Authorization: Bearer ...`.
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// Environment variable holding the bearer token.
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    /// Extra request headers.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Limit for the whole request.
    #[serde(default = "default_qido_timeout")]
    pub timeout: DurationText,
    /// PEM file with extra trusted certificate authorities.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    /// Largest accepted response body.
    #[serde(default = "default_qido_max_response_size")]
    pub max_response_size: usize,
}

/// Settings of the `dicomweb-wado` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WadoSettings {
    /// Base URL of the DICOMweb service.
    pub url: String,
    /// The inbox of the `dicom-retrieved` source that stores the instances.
    pub into: String,
    /// Bearer token sent as `Authorization: Bearer ...`.
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// Environment variable holding the bearer token.
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    /// Extra request headers.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Limit for the whole request, including storing the instances.
    #[serde(default = "default_wado_timeout")]
    pub timeout: DurationText,
    /// PEM file with extra trusted certificate authorities.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    /// Largest accepted response body; instances are held in memory.
    #[serde(default = "default_wado_max_response_size")]
    pub max_response_size: usize,
}

/// Percent-encodes a query parameter value (RFC 3986 unreserved kept).
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'*' | b'?' | b',')
        {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// How an HTTP status answers a delivery: `408`, `429`, `3xx` and `5xx`
/// are retried, other non-success statuses fail it.
fn http_failure(status: StatusCode, body: &[u8]) -> SendError {
    let detail = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
    let message = format!("HTTP {status}: {detail}");
    let retry = status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || status.is_redirection();
    if retry {
        SendError::temporary(message)
    } else {
        SendError::permanent(message)
    }
}

struct Http {
    base: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    timeout: Duration,
    max_response_size: usize,
    client: HttpClient,
}

impl Http {
    async fn get(
        &self,
        url: &str,
        accept: &'static str,
        message_id: &str,
    ) -> Result<(StatusCode, Option<String>, Bytes), SendError> {
        let uri: Uri = url
            .parse()
            .map_err(|e| SendError::permanent(format!("invalid request URL {url:?}: {e}")))?;
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(ACCEPT, HeaderValue::from_static(accept))
            .header("X-OXIM-Message-Id", message_id);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(Full::new(Bytes::new()))
            .map_err(|e| SendError::permanent(format!("cannot build the request: {e}")))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|e| SendError::temporary(format!("request to {url} failed: {e}")))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = Limited::new(response.into_body(), self.max_response_size)
            .collect()
            .await
            .map_err(|e| SendError::temporary(format!("reading the response failed: {e}")))?
            .to_bytes();
        Ok((status, content_type, body))
    }
}

/// A QIDO-RS search destination.
pub struct QidoDestination {
    http: Http,
    level: QidoLevel,
    params: BTreeMap<String, String>,
}

impl std::fmt::Debug for QidoDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QidoDestination")
            .field("base", &self.http.base)
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

impl QidoDestination {
    /// Validates the settings and creates the destination.
    pub fn new(settings: QidoSettings) -> Result<Self, EngineError> {
        const WHAT: &str = "dicomweb-qido destination";
        Ok(Self {
            http: Http {
                base: web::base_url(WHAT, &settings.url)?,
                headers: web::headers(
                    WHAT,
                    &settings.headers,
                    settings.bearer_token.as_ref(),
                    settings.bearer_token_env.as_ref(),
                )?,
                timeout: settings.timeout.0,
                max_response_size: settings.max_response_size,
                client: web::client(settings.ca_file.as_ref())?,
            },
            level: settings.level,
            params: settings.params,
        })
    }

    /// The search URL for an identifier: keys with values become matching
    /// parameters, keys without values become `includefield`.
    pub fn url(&self, identifier: &dicom_object::InMemDicomObject) -> String {
        let mut params: Vec<(String, String)> = self
            .params
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for element in identifier.iter() {
            let tag = element.header().tag();
            if tag == tags::QUERY_RETRIEVE_LEVEL || tag == tags::SPECIFIC_CHARACTER_SET {
                continue;
            }
            let name = StandardDataDictionary
                .by_tag(tag)
                .map(|entry| entry.alias().to_owned())
                .unwrap_or_else(|| format!("{:04X}{:04X}", tag.group(), tag.element()));
            match value_text(element.value()).filter(|value| !value.is_empty()) {
                Some(value) => params.push((name, value.replace('\\', ","))),
                None => params.push(("includefield".to_owned(), name)),
            }
        }
        let query: Vec<String> = params
            .iter()
            .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
            .collect();
        let mut url = format!("{}/{}", self.http.base, self.level.path());
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        url
    }
}

#[async_trait]
impl DestinationConnector for QidoDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let identifier =
            parse_identifier(&delivery.payload).map_err(|e| SendError::permanent(e.to_string()))?;
        let url = self.url(&identifier);
        let (status, _, body) = tokio::time::timeout(
            self.http.timeout,
            self.http.get(
                &url,
                "application/dicom+json",
                &delivery.message_id.to_string(),
            ),
        )
        .await
        .map_err(|_| {
            SendError::temporary(format!("no response from {url} within the timeout"))
        })??;
        match status {
            StatusCode::NO_CONTENT => Ok(Some(b"[]".to_vec())),
            status if status.is_success() => Ok(Some(body.to_vec())),
            status => Err(http_failure(status, &body)),
        }
    }
}

/// A WADO-RS retrieve destination.
pub struct WadoDestination {
    http: Http,
    into: String,
    environment: DicomEnvironment,
}

impl std::fmt::Debug for WadoDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WadoDestination")
            .field("base", &self.http.base)
            .field("into", &self.into)
            .finish_non_exhaustive()
    }
}

impl WadoDestination {
    /// Validates the settings and creates the destination.
    pub fn new(settings: WadoSettings, environment: DicomEnvironment) -> Result<Self, EngineError> {
        const WHAT: &str = "dicomweb-wado destination";
        if settings.into.trim().is_empty() {
            return Err(EngineError::Config(format!(
                "{WHAT}: into (the inbox of a dicom-retrieved source) must not be empty"
            )));
        }
        Ok(Self {
            http: Http {
                base: web::base_url(WHAT, &settings.url)?,
                headers: web::headers(
                    WHAT,
                    &settings.headers,
                    settings.bearer_token.as_ref(),
                    settings.bearer_token_env.as_ref(),
                )?,
                timeout: settings.timeout.0,
                max_response_size: settings.max_response_size,
                client: web::client(settings.ca_file.as_ref())?,
            },
            into: settings.into.trim().to_owned(),
            environment,
        })
    }

    /// The retrieve URL for the UIDs of an identifier.
    pub fn url(&self, identifier: &dicom_object::InMemDicomObject) -> Result<String, String> {
        let study = text(identifier, tags::STUDY_INSTANCE_UID)
            .ok_or("the identifier has no StudyInstanceUID")?;
        let mut url = format!("{}/studies/{}", self.http.base, encode(&study));
        if let Some(series) = text(identifier, tags::SERIES_INSTANCE_UID) {
            url.push_str(&format!("/series/{}", encode(&series)));
            if let Some(instance) = text(identifier, tags::SOP_INSTANCE_UID) {
                url.push_str(&format!("/instances/{}", encode(&instance)));
            }
        } else if text(identifier, tags::SOP_INSTANCE_UID).is_some() {
            return Err("an instance needs its SeriesInstanceUID".into());
        }
        Ok(url)
    }

    async fn retrieve(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let identifier =
            parse_identifier(&delivery.payload).map_err(|e| SendError::permanent(e.to_string()))?;
        let url = self.url(&identifier).map_err(SendError::permanent)?;
        let (status, content_type, body) = self
            .http
            .get(
                &url,
                "multipart/related; type=\"application/dicom\"; transfer-syntax=*",
                &delivery.message_id.to_string(),
            )
            .await?;
        if !status.is_success() {
            return Err(http_failure(status, &body));
        }
        let content_type = content_type.unwrap_or_default();
        let boundary = web::media_parameter(&content_type, "boundary").ok_or_else(|| {
            SendError::permanent(format!(
                "the response is not multipart/related: {content_type:?}"
            ))
        })?;
        let parts = web::multipart_parts(&body, boundary).map_err(SendError::permanent)?;
        let mut stored = Vec::with_capacity(parts.len());
        for part in parts {
            let meta = part10::parse(part)
                .map_err(|e| {
                    SendError::permanent(format!("a retrieved part is not a DICOM object: {e}"))
                })?
                .meta;
            let metadata = BTreeMap::from([
                ("dicom.command".to_owned(), "WADO-RS".to_owned()),
                ("dicom.sop_class_uid".to_owned(), meta.sop_class_uid.clone()),
                (
                    "dicom.sop_instance_uid".to_owned(),
                    meta.sop_instance_uid.clone(),
                ),
                (
                    "dicom.transfer_syntax".to_owned(),
                    meta.transfer_syntax.clone(),
                ),
            ]);
            let id = self
                .environment
                .deliver(
                    &self.into,
                    part.to_vec(),
                    metadata,
                    Some(url.clone()),
                    self.http.timeout,
                )
                .await
                .map_err(SendError::from)?;
            stored.push(id.to_string());
        }
        debug!(%url, instances = stored.len(), "WADO-RS retrieved");
        let body = serde_json::json!({
            "retrieved": stored.len(),
            "message_ids": stored,
        });
        Ok(Some(body.to_string().into_bytes()))
    }
}

#[async_trait]
impl DestinationConnector for WadoDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        tokio::time::timeout(self.http.timeout, self.retrieve(delivery))
            .await
            .map_err(|_| SendError::temporary("WADO-RS did not finish within the timeout"))?
    }
}

/// Registers the DICOMweb client destinations.
pub(crate) fn register(registry: &mut oxim_core::Registry, environment: &DicomEnvironment) {
    registry.add_destination("dicomweb-qido", |config: &DestinationConfig| {
        let settings: QidoSettings = settings(&config.settings, "dicomweb-qido destination")?;
        Ok(Arc::new(QidoDestination::new(settings)?) as Arc<dyn DestinationConnector>)
    });
    let environment = environment.clone();
    registry.add_destination("dicomweb-wado", move |config: &DestinationConfig| {
        let settings: WadoSettings = settings(&config.settings, "dicomweb-wado destination")?;
        Ok(
            Arc::new(WadoDestination::new(settings, environment.clone())?)
                as Arc<dyn DestinationConnector>,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse_identifier;

    fn qido() -> QidoDestination {
        QidoDestination::new(QidoSettings {
            url: "https://pacs.example/dicom-web/".into(),
            level: QidoLevel::Studies,
            params: BTreeMap::from([("limit".to_owned(), "10".to_owned())]),
            bearer_token: None,
            bearer_token_env: None,
            headers: BTreeMap::new(),
            timeout: default_qido_timeout(),
            ca_file: None,
            max_response_size: default_qido_max_response_size(),
        })
        .unwrap()
    }

    #[test]
    fn builds_search_urls() {
        let identifier = parse_identifier(
            br#"{"PatientID": "SYN 0001", "StudyDate": "20240101-20240131", "ModalitiesInStudy": ["CT", "MR"], "StudyInstanceUID": null}"#,
        )
        .unwrap();
        assert_eq!(
            qido().url(&identifier),
            "https://pacs.example/dicom-web/studies?limit=10&StudyDate=20240101-20240131&ModalitiesInStudy=CT,MR&PatientID=SYN%200001&includefield=StudyInstanceUID"
        );
    }

    #[test]
    fn builds_retrieve_urls() {
        let wado = WadoDestination::new(
            WadoSettings {
                url: "http://pacs.example/wado".into(),
                into: "retrieved".into(),
                bearer_token: None,
                bearer_token_env: None,
                headers: BTreeMap::new(),
                timeout: default_wado_timeout(),
                ca_file: None,
                max_response_size: default_wado_max_response_size(),
            },
            DicomEnvironment::in_memory(),
        )
        .unwrap();
        let url = |json: &str| wado.url(&parse_identifier(json.as_bytes()).unwrap());
        assert_eq!(
            url(r#"{"StudyInstanceUID": "1.2.3"}"#).unwrap(),
            "http://pacs.example/wado/studies/1.2.3"
        );
        assert_eq!(
            url(r#"{"StudyInstanceUID": "1.2.3", "SeriesInstanceUID": "1.2.3.4", "SOPInstanceUID": "1.2.3.4.5"}"#)
                .unwrap(),
            "http://pacs.example/wado/studies/1.2.3/series/1.2.3.4/instances/1.2.3.4.5"
        );
        assert!(url(r#"{"PatientID": "X"}"#).is_err());
        assert!(url(r#"{"StudyInstanceUID": "1.2", "SOPInstanceUID": "1.2.3"}"#).is_err());
    }
}
