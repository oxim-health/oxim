//! OAuth 2.0 client credentials (RFC 6749 §4.4) for HTTP-based
//! destinations, with token caching and refresh.
//!
//! An `oauth2` settings block:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `token_url` | required | Token endpoint (`https://`; `http://` only for loopback addresses) |
//! | `client_id` | required | Client identifier |
//! | `client_secret_env` | none | Environment variable holding the client secret |
//! | `client_secret` | none | The secret inline (discouraged; use `client_secret_env`) |
//! | `scope` | none | Space-separated scopes |
//! | `audience` | none | `audience` parameter some providers require |
//! | `client_auth` | `basic` | `basic` (HTTP Basic, RFC 6749 §2.3.1) or `body` (credentials in the form) |
//! | `tls` | system roots | TLS settings for the token endpoint; see [`tls`](crate::tls) |
//! | `timeout` | `30s` | Limit for a token request |
//!
//! The token is requested when first needed and reused until 30 seconds
//! before it expires (`expires_in`; one hour when the server does not say).
//! A `401` from the protected endpoint discards the token so the next
//! attempt fetches a new one. Token endpoint failures are temporary delivery
//! failures: the message is not at fault, and deliveries resume once the
//! credentials or the provider are fixed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use http::{Method, Request, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use oxim_core::config::DurationText;
use oxim_core::{EngineError, SendError};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::http_listener::base64;
use crate::tls::{ClientTlsSettings, client_config};

/// How the client authenticates to the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAuth {
    /// HTTP Basic authentication with the URL-encoded id and secret.
    #[default]
    Basic,
    /// `client_id` and `client_secret` in the request body.
    Body,
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

/// Settings of OAuth 2.0 client credentials.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuth2Settings {
    /// Token endpoint.
    pub token_url: String,
    /// Client identifier.
    pub client_id: String,
    /// Environment variable holding the client secret.
    #[serde(default)]
    pub client_secret_env: Option<String>,
    /// The client secret inline (discouraged).
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Space-separated scopes.
    #[serde(default)]
    pub scope: Option<String>,
    /// Audience parameter.
    #[serde(default)]
    pub audience: Option<String>,
    /// Client authentication method.
    #[serde(default)]
    pub client_auth: ClientAuth,
    /// TLS for the token endpoint.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Limit for a token request.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// The token of the last successful request.
#[derive(Clone)]
struct Cached {
    header: HeaderValue,
    refresh_after: Instant,
}

/// Obtains and caches access tokens.
pub struct OAuth2Client {
    settings: OAuth2Settings,
    uri: Uri,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    cached: Mutex<Option<Cached>>,
}

impl std::fmt::Debug for OAuth2Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2Client")
            .field("token_url", &self.settings.token_url)
            .field("client_id", &self.settings.client_id)
            .finish_non_exhaustive()
    }
}

/// `application/x-www-form-urlencoded` encoding of one value.
pub(crate) fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn is_loopback(uri: &Uri) -> bool {
    matches!(
        uri.host(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    )
}

impl OAuth2Client {
    /// Validates the settings. The secret is read from the environment when
    /// the first token is requested, so validating a channel does not need
    /// it.
    pub fn new(settings: OAuth2Settings) -> Result<Arc<Self>, EngineError> {
        let error = |message: String| EngineError::Config(format!("oauth2: {message}"));
        let uri: Uri = settings
            .token_url
            .parse()
            .map_err(|e| error(format!("invalid token_url {:?}: {e}", settings.token_url)))?;
        match uri.scheme_str() {
            Some("https") => {}
            Some("http") if is_loopback(&uri) => {}
            _ => {
                return Err(error(format!(
                    "token_url {:?} must be an https:// URL",
                    settings.token_url
                )));
            }
        }
        if settings.client_secret.is_some() == settings.client_secret_env.is_some() {
            return Err(error(
                "set exactly one of client_secret_env and client_secret".into(),
            ));
        }
        let tls = settings.tls.clone().unwrap_or_default();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(client_config(&tls)?)
            .https_or_http()
            .enable_http1()
            .build();
        Ok(Arc::new(Self {
            settings,
            uri,
            client: Client::builder(TokioExecutor::new()).build(connector),
            cached: Mutex::new(None),
        }))
    }

    fn secret(&self) -> Result<String, SendError> {
        match (
            &self.settings.client_secret,
            &self.settings.client_secret_env,
        ) {
            (Some(secret), _) => Ok(secret.clone()),
            (None, Some(name)) => std::env::var(name).map_err(|_| {
                SendError::temporary(format!(
                    "oauth2: the environment variable {name} holding the client secret is not set"
                ))
            }),
            (None, None) => Err(SendError::temporary("oauth2: no client secret")),
        }
    }

    /// The `Authorization` header value, from the cache or a new token.
    pub async fn authorization(&self) -> Result<HeaderValue, SendError> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref()
            && Instant::now() < token.refresh_after
        {
            return Ok(token.header.clone());
        }
        let token = tokio::time::timeout(self.settings.timeout.0, self.fetch())
            .await
            .map_err(|_| SendError::temporary("oauth2: the token request timed out"))??;
        let header = token.header.clone();
        *cached = Some(token);
        Ok(header)
    }

    /// Discards the cached token, for example after a `401`.
    pub async fn invalidate(&self) {
        *self.cached.lock().await = None;
    }

    async fn fetch(&self) -> Result<Cached, SendError> {
        let settings = &self.settings;
        let secret = self.secret()?;
        let mut form = vec!["grant_type=client_credentials".to_owned()];
        if let Some(scope) = &settings.scope {
            form.push(format!("scope={}", form_encode(scope)));
        }
        if let Some(audience) = &settings.audience {
            form.push(format!("audience={}", form_encode(audience)));
        }
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(self.uri.clone())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json");
        match settings.client_auth {
            ClientAuth::Basic => {
                let pair = format!(
                    "{}:{}",
                    form_encode(&settings.client_id),
                    form_encode(&secret)
                );
                request =
                    request.header(AUTHORIZATION, format!("Basic {}", base64(pair.as_bytes())));
            }
            ClientAuth::Body => {
                form.push(format!("client_id={}", form_encode(&settings.client_id)));
                form.push(format!("client_secret={}", form_encode(&secret)));
            }
        }
        let request = request
            .body(Full::new(Bytes::from(form.join("&"))))
            .map_err(|e| SendError::temporary(format!("oauth2: cannot build the request: {e}")))?;
        let response = self.client.request(request).await.map_err(|e| {
            SendError::temporary(format!("oauth2: token request to {} failed: {e}", self.uri))
        })?;
        let status = response.status();
        let body = Limited::new(response.into_body(), 64 * 1024)
            .collect()
            .await
            .map_err(|e| SendError::temporary(format!("oauth2: reading the token failed: {e}")))?
            .to_bytes();
        if !status.is_success() {
            let detail = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
            return Err(SendError::temporary(format!(
                "oauth2: the token endpoint answered {status}: {detail}"
            )));
        }
        let token: TokenResponse = serde_json::from_slice(&body)
            .map_err(|e| SendError::temporary(format!("oauth2: invalid token response: {e}")))?;
        if let Some(kind) = &token.token_type
            && !kind.eq_ignore_ascii_case("bearer")
        {
            return Err(SendError::temporary(format!(
                "oauth2: unsupported token type {kind:?}"
            )));
        }
        let lifetime = Duration::from_secs(token.expires_in.unwrap_or(3600));
        let margin = Duration::from_secs(30).min(lifetime / 2);
        let mut header = HeaderValue::from_str(&format!("Bearer {}", token.access_token))
            .map_err(|_| SendError::temporary("oauth2: the access token is not a valid header"))?;
        header.set_sensitive(true);
        Ok(Cached {
            header,
            refresh_after: Instant::now() + lifetime - margin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_form_values() {
        assert_eq!(form_encode("read write"), "read+write");
        assert_eq!(form_encode("a&b=c/d:é"), "a%26b%3Dc%2Fd%3A%C3%A9");
        assert_eq!(form_encode("A-z_0.9*"), "A-z_0.9*");
    }

    fn settings(token_url: &str) -> OAuth2Settings {
        OAuth2Settings {
            token_url: token_url.into(),
            client_id: "lab".into(),
            client_secret_env: None,
            client_secret: Some("secret".into()),
            scope: None,
            audience: None,
            client_auth: ClientAuth::Basic,
            tls: None,
            timeout: default_timeout(),
        }
    }

    #[test]
    fn checks_the_settings() {
        assert!(OAuth2Client::new(settings("https://idp.example.org/token")).is_ok());
        assert!(OAuth2Client::new(settings("http://127.0.0.1:9/token")).is_ok());
        assert!(OAuth2Client::new(settings("http://idp.example.org/token")).is_err());
        let mut both = settings("https://idp.example.org/token");
        both.client_secret_env = Some("X".into());
        assert!(OAuth2Client::new(both).is_err());
    }
}
