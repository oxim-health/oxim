//! Receiving messages over HTTP and HTTPS (REST clients, webhooks).
//!
//! Source type `http`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `listen` | required | Address to listen on, for example `0.0.0.0:8081` |
//! | `path` | `/` | Accepted path prefix; other paths get `404` |
//! | `methods` | `[POST, PUT]` | Accepted methods; others get `405` |
//! | `max_body` | 16 MiB | Largest accepted body; larger bodies get `413` |
//! | `max_connections` | `100` | Concurrent connections |
//! | `status` | `200` | Status of the answer once a message is stored |
//! | `metadata_headers` | none | Request headers copied into the message metadata as `http.header.<name>` |
//! | `auth` | none | `{type: basic, username, password_env}` or `{type: bearer, token_env}` (`password`/`token` inline are accepted but discouraged) |
//! | `tls` | none | TLS listener settings; see [`tls`](crate::tls) |
//!
//! The body is the message. Once it is stored durably the client gets
//! `status` with `{"message_id": "..."}` and an `X-OXIM-Message-Id`
//! header; if it cannot be stored the client gets `503` and should retry.
//! When the channel answers requests (`source.response`), the reply is the
//! response body, with the media type of its data type; a request without
//! a reply gets the JSON answer, with status `500` when processing failed.
//! The `X-Correlation-ID` request header becomes the correlation
//! identifier, and `http.method`, `http.path` and `http.content_type` are
//! recorded as metadata.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{ALLOW, CONTENT_TYPE, HeaderValue, WWW_AUTHENTICATE};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use oxim_core::{
    ConnectorError, EngineError, Registry, SourceConfig, SourceConnector, SourceContext,
    SubmitInfo, async_trait,
};
use oxim_model::MessageStatus;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::http::content_type;
use crate::net::{DEFAULT_MAX_MESSAGE, ListenerTls, serve, settings};
use crate::tls::{ServerTlsSettings, Stream};

fn default_path() -> String {
    "/".to_owned()
}

fn default_methods() -> Vec<String> {
    vec!["POST".to_owned(), "PUT".to_owned()]
}

fn default_max_body() -> usize {
    DEFAULT_MAX_MESSAGE
}

fn default_max_connections() -> usize {
    100
}

fn default_status() -> u16 {
    200
}

/// How clients authenticate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpAuth {
    /// HTTP Basic authentication.
    Basic {
        /// The expected user name.
        username: String,
        /// The password, inline (discouraged).
        #[serde(default)]
        password: Option<String>,
        /// The environment variable holding the password.
        #[serde(default)]
        password_env: Option<String>,
    },
    /// A bearer token.
    Bearer {
        /// The token, inline (discouraged).
        #[serde(default)]
        token: Option<String>,
        /// The environment variable holding the token.
        #[serde(default)]
        token_env: Option<String>,
    },
}

/// Settings of the `http` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpSourceSettings {
    /// Address to listen on.
    pub listen: String,
    /// Accepted path prefix.
    #[serde(default = "default_path")]
    pub path: String,
    /// Accepted methods.
    #[serde(default = "default_methods")]
    pub methods: Vec<String>,
    /// Largest accepted body.
    #[serde(default = "default_max_body")]
    pub max_body: usize,
    /// Concurrent connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Status of the answer once a message is stored.
    #[serde(default = "default_status")]
    pub status: u16,
    /// Request headers recorded as metadata.
    #[serde(default)]
    pub metadata_headers: Vec<String>,
    /// Client authentication.
    #[serde(default)]
    pub auth: Option<HttpAuth>,
    /// TLS, optionally with client certificates (mutual TLS).
    #[serde(default)]
    pub tls: Option<ServerTlsSettings>,
}

/// The expected `Authorization` header value.
#[derive(Clone)]
struct Credentials(Vec<u8>);

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(..)")
    }
}

fn secret(inline: Option<&String>, env: Option<&String>, what: &str) -> Result<String, String> {
    match (inline, env) {
        (Some(value), None) => Ok(value.clone()),
        (None, Some(name)) => std::env::var(name)
            .map_err(|_| format!("the environment variable {name} holding the {what} is not set")),
        _ => Err(format!(
            "set exactly one of the {what} and its environment variable"
        )),
    }
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl HttpAuth {
    fn credentials(&self) -> Result<Credentials, String> {
        match self {
            Self::Basic {
                username,
                password,
                password_env,
            } => {
                let password = secret(password.as_ref(), password_env.as_ref(), "password")?;
                let pair = format!("{username}:{password}");
                Ok(Credentials(
                    format!("Basic {}", base64(pair.as_bytes())).into_bytes(),
                ))
            }
            Self::Bearer { token, token_env } => {
                let token = secret(token.as_ref(), token_env.as_ref(), "token")?;
                Ok(Credentials(format!("Bearer {token}").into_bytes()))
            }
        }
    }

    fn challenge(&self) -> &'static str {
        match self {
            Self::Basic { .. } => "Basic realm=\"oxim\"",
            Self::Bearer { .. } => "Bearer",
        }
    }
}

/// Compares without an early exit, so timing does not reveal how much of
/// a credential matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    let mut difference = u8::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        difference |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0xff);
    }
    difference == 0
}

/// Receives messages over HTTP.
#[derive(Debug, Clone)]
pub struct HttpSource {
    settings: Arc<HttpSourceSettings>,
    methods: Arc<Vec<Method>>,
    status: StatusCode,
    tls: Option<ListenerTls>,
}

impl HttpSource {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: HttpSourceSettings) -> Result<Self, EngineError> {
        let methods = settings
            .methods
            .iter()
            .map(|method| {
                Method::from_bytes(method.to_ascii_uppercase().as_bytes()).map_err(|_| {
                    EngineError::Config(format!("http source: invalid method {method:?}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if methods.is_empty() {
            return Err(EngineError::Config(
                "http source: methods must not be empty".into(),
            ));
        }
        let status = StatusCode::from_u16(settings.status)
            .ok()
            .filter(StatusCode::is_success)
            .ok_or_else(|| {
                EngineError::Config(format!(
                    "http source: status must be a 2xx code, not {}",
                    settings.status
                ))
            })?;
        if !settings.path.starts_with('/') {
            return Err(EngineError::Config(format!(
                "http source: path must start with '/', not {:?}",
                settings.path
            )));
        }
        let tls = ListenerTls::from_settings(settings.tls.as_ref())?;
        Ok(Self {
            settings: Arc::new(settings),
            methods: Arc::new(methods),
            status,
            tls,
        })
    }
}

#[derive(Debug)]
struct Shared {
    context: SourceContext,
    settings: Arc<HttpSourceSettings>,
    methods: Arc<Vec<Method>>,
    status: StatusCode,
    credentials: Option<(Credentials, &'static str)>,
}

#[async_trait]
impl SourceConnector for HttpSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let credentials = self
            .settings
            .auth
            .as_ref()
            .map(|auth| auth.credentials().map(|c| (c, auth.challenge())))
            .transpose()
            .map_err(|e| ConnectorError(format!("http source authentication: {e}")))?;
        let shared = Arc::new(Shared {
            context: context.clone(),
            settings: self.settings.clone(),
            methods: self.methods.clone(),
            status: self.status,
            credentials,
        });
        serve(
            &context,
            &self.settings.listen,
            self.settings.max_connections,
            self.tls.clone(),
            move |stream, peer| connection(shared.clone(), stream, peer),
        )
        .await
    }
}

async fn connection(shared: Arc<Shared>, stream: Stream, peer: SocketAddr) {
    let service = {
        let shared = shared.clone();
        service_fn(move |request| {
            let shared = shared.clone();
            async move { Ok::<_, Infallible>(handle(&shared, request, peer).await) }
        })
    };
    let connection = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30))
        .serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    tokio::select! {
        result = connection.as_mut() => {
            if let Err(e) = result {
                debug!(channel = %shared.context.channel(), %peer, error = %e, "HTTP connection ended");
            }
        }
        () = shared.context.cancelled() => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }
    }
}

fn json(status: StatusCode, value: &serde_json::Value) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn error(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    json(status, &serde_json::json!({ "error": message }))
}

async fn handle(
    shared: &Shared,
    request: Request<Incoming>,
    peer: SocketAddr,
) -> Response<Full<Bytes>> {
    let settings = &shared.settings;
    if !request.uri().path().starts_with(&settings.path) {
        return error(StatusCode::NOT_FOUND, "no such path");
    }
    if !shared.methods.contains(request.method()) {
        let mut response = error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        let allow = shared
            .methods
            .iter()
            .map(Method::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if let Ok(value) = HeaderValue::from_str(&allow) {
            response.headers_mut().insert(ALLOW, value);
        }
        return response;
    }
    if let Some((expected, challenge)) = &shared.credentials {
        let given = request
            .headers()
            .get(http::header::AUTHORIZATION)
            .map(HeaderValue::as_bytes)
            .unwrap_or_default();
        if !same(given, &expected.0) {
            let mut response = error(StatusCode::UNAUTHORIZED, "authentication required");
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
            return response;
        }
    }
    let mut info = SubmitInfo {
        peer: Some(peer.to_string()),
        ..SubmitInfo::default()
    };
    let headers = request.headers();
    info.correlation_id = headers
        .get("x-correlation-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut metadata = BTreeMap::new();
    metadata.insert("http.method".to_owned(), request.method().to_string());
    metadata.insert(
        "http.path".to_owned(),
        request.uri().path_and_query().map_or_else(
            || request.uri().path().to_owned(),
            |p| p.as_str().to_owned(),
        ),
    );
    if let Some(value) = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        metadata.insert("http.content_type".to_owned(), value.to_owned());
    }
    for name in &settings.metadata_headers {
        if let Some(value) = headers.get(name.as_str()).and_then(|v| v.to_str().ok()) {
            metadata.insert(
                format!("http.header.{}", name.to_ascii_lowercase()),
                value.to_owned(),
            );
        }
    }
    info.metadata = metadata;

    let body = match Limited::new(request.into_body(), settings.max_body)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(e) => {
            return if e.is::<http_body_util::LengthLimitError>() {
                error(StatusCode::PAYLOAD_TOO_LARGE, "the body is too large")
            } else {
                error(StatusCode::BAD_REQUEST, "the body could not be read")
            };
        }
    };
    let context = &shared.context;
    if context.responds() {
        return match context.request(body.to_vec(), info).await {
            Ok(reply) => {
                let id = reply.message_id.to_string();
                let mut response = match reply.data {
                    Some(data) => {
                        let mut response = Response::new(Full::new(Bytes::from(data)));
                        response.headers_mut().insert(
                            CONTENT_TYPE,
                            HeaderValue::from_static(content_type(reply.data_type)),
                        );
                        response
                    }
                    None => {
                        let status = if reply.status == MessageStatus::Error {
                            StatusCode::INTERNAL_SERVER_ERROR
                        } else {
                            shared.status
                        };
                        json(
                            status,
                            &serde_json::json!({
                                "message_id": id,
                                "status": reply.status,
                                "error": reply.error,
                            }),
                        )
                    }
                };
                if let Ok(value) = HeaderValue::from_str(&id) {
                    response.headers_mut().insert("x-oxim-message-id", value);
                }
                response
            }
            Err(e) => {
                warn!(channel = %context.channel(), %peer, error = %e, "message could not be stored");
                error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the message could not be stored",
                )
            }
        };
    }
    match context.submit(body.to_vec(), info).await {
        Ok(id) => {
            let id = id.to_string();
            let mut response = json(shared.status, &serde_json::json!({ "message_id": id }));
            if let Ok(value) = HeaderValue::from_str(&id) {
                response.headers_mut().insert("x-oxim-message-id", value);
            }
            response
        }
        Err(e) => {
            warn!(channel = %context.channel(), %peer, error = %e, "message could not be stored");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "the message could not be stored",
            )
        }
    }
}

/// Registers the `http` source.
pub fn register(registry: &mut Registry) {
    registry.add_source("http", |config: &SourceConfig| {
        let settings: HttpSourceSettings = settings(&config.settings, "http source")?;
        Ok(Arc::new(HttpSource::new(settings)?) as Arc<dyn SourceConnector>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"lab:secret"), "bGFiOnNlY3JldA==");
    }

    #[test]
    fn compares_credentials() {
        assert!(same(b"Bearer abc", b"Bearer abc"));
        assert!(!same(b"Bearer abd", b"Bearer abc"));
        assert!(!same(b"Bearer ab", b"Bearer abc"));
        assert!(!same(b"", b"x"));
    }

    #[test]
    fn needs_exactly_one_secret() {
        let auth = HttpAuth::Bearer {
            token: None,
            token_env: None,
        };
        assert!(auth.credentials().is_err());
        let auth = HttpAuth::Bearer {
            token: Some("t".into()),
            token_env: Some("X".into()),
        };
        assert!(auth.credentials().is_err());
        let auth = HttpAuth::Basic {
            username: "lab".into(),
            password: None,
            password_env: Some("OXIM_TEST_UNSET_VARIABLE_1F3A".into()),
        };
        let error = auth.credentials().unwrap_err();
        assert!(error.contains("OXIM_TEST_UNSET_VARIABLE_1F3A"), "{error}");
        let auth = HttpAuth::Basic {
            username: "lab".into(),
            password: Some("secret".into()),
            password_env: None,
        };
        assert_eq!(auth.credentials().unwrap().0, b"Basic bGFiOnNlY3JldA==");
    }
}
