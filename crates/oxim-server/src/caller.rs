//! Request authentication: bearer tokens and session cookies with CSRF
//! protection.

use std::net::SocketAddr;

use axum::extract::connect_info::Connected;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, header};
use axum::serve::IncomingStream;
use oxim_auth::{API_TOKEN_PREFIX, Permission, Principal, SESSION_PREFIX};

use crate::error::ApiError;
use crate::state::AppState;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "oxim_session";
/// Name of the cookie holding the CSRF token (readable by the UI).
pub const CSRF_COOKIE: &str = "oxim_csrf";
/// Header in which cookie-authenticated changes repeat the CSRF token.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The value of a cookie in the request.
pub(crate) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// The address of the connected client, recorded by [`crate::serve`] for
/// both HTTP and HTTPS connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClientAddr(pub(crate) SocketAddr);

impl Connected<IncomingStream<'_, tokio::net::TcpListener>> for ClientAddr {
    fn connect_info(stream: IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

impl Connected<IncomingStream<'_, crate::tls::TlsListener>> for ClientAddr {
    fn connect_info(stream: IncomingStream<'_, crate::tls::TlsListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

/// The client address, when the server records it.
pub(crate) fn client_address(parts: &Parts) -> Option<String> {
    let extensions = &parts.extensions;
    extensions
        .get::<ConnectInfo<ClientAddr>>()
        .map(|ConnectInfo(ClientAddr(address))| *address)
        .or_else(|| {
            extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(address)| *address)
        })
        .map(|address| address.ip().to_string())
}

/// An authenticated request.
#[derive(Debug, Clone)]
pub struct Caller {
    /// Who is calling.
    pub principal: Principal,
    /// The client address, if known.
    pub client: Option<String>,
    /// The session token, for session requests (used by logout).
    pub(crate) session_token: Option<String>,
}

impl Caller {
    /// Fails with `403` unless the caller holds `permission`.
    pub fn require(&self, permission: Permission) -> Result<(), ApiError> {
        if self.principal.can(permission) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!(
                "the {} role does not allow this",
                self.principal.role
            )))
        }
    }

    /// The name recorded in the audit trail.
    pub fn actor(&self) -> String {
        self.principal.username.clone()
    }
}

fn unsafe_method(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let auth = &state.inner.auth;
        let now = state.inner.engine.clock().now();
        let client = client_address(parts);
        let bearer = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim);
        if let Some(token) = bearer {
            // Bearer tokens are never sent automatically by browsers, so they
            // need no CSRF protection.
            if token.starts_with(API_TOKEN_PREFIX) {
                let principal = auth.api_token(token, now)?;
                return Ok(Self {
                    principal,
                    client,
                    session_token: None,
                });
            }
            if token.starts_with(SESSION_PREFIX) {
                let session = auth.session(token, now)?;
                return Ok(Self {
                    principal: session.principal,
                    client,
                    session_token: Some(token.to_owned()),
                });
            }
            return Err(ApiError::unauthenticated());
        }
        let Some(token) = cookie(&parts.headers, SESSION_COOKIE) else {
            return Err(ApiError::unauthenticated());
        };
        let session = auth.session(token, now)?;
        if unsafe_method(&parts.method) {
            let sent = parts
                .headers
                .get(CSRF_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if sent.is_empty() || !session.csrf_matches(sent) {
                return Err(ApiError::forbidden(
                    "missing or wrong CSRF token; send the oxim_csrf cookie value in the X-CSRF-Token header",
                ));
            }
        }
        Ok(Self {
            principal: session.principal,
            client,
            session_token: Some(token.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn reads_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; oxim_session=oxs_abc; oxim_csrf=xyz"),
        );
        assert_eq!(cookie(&headers, SESSION_COOKIE), Some("oxs_abc"));
        assert_eq!(cookie(&headers, CSRF_COOKIE), Some("xyz"));
        assert_eq!(cookie(&headers, "missing"), None);
    }
}
