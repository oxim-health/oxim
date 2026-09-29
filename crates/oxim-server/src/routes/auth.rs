//! Login, logout and the current user.

use std::time::Instant;

use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use oxim_auth::{AuthError, LoginThrottle, Principal};
use serde::Deserialize;
use serde_json::json;

use crate::audit;
use crate::caller::{CSRF_COOKIE, Caller, SESSION_COOKIE, client_address};
use crate::error::{ApiError, ApiResult};
use crate::extract::JsonBody;
use crate::state::AppState;

/// The client address of a request, for extractors that do not
/// authenticate.
pub(crate) struct ClientAddress(pub(crate) Option<String>);

impl FromRequestParts<AppState> for ClientAddress {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, ApiError> {
        Ok(Self(client_address(parts)))
    }
}

/// The public view of a principal.
pub(crate) fn principal_json(principal: &Principal) -> serde_json::Value {
    json!({
        "username": principal.username,
        "display_name": principal.display_name,
        "role": principal.role,
        "kind": principal.kind,
        "permissions": principal.role.permissions(),
    })
}

fn cookie_header(
    name: &str,
    value: &str,
    max_age: u64,
    http_only: bool,
    secure: bool,
) -> Option<HeaderValue> {
    let mut cookie = format!("{name}={value}; Path=/; SameSite=Strict; Max-Age={max_age}");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).ok()
}

fn clear_cookies(headers: &mut HeaderMap, secure: bool) {
    for (name, http_only) in [(SESSION_COOKIE, true), (CSRF_COOKIE, false)] {
        if let Some(value) = cookie_header(name, "", 0, http_only, secure) {
            headers.append(header::SET_COOKIE, value);
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct LoginRequest {
    username: String,
    password: String,
}

/// `POST /auth/login`
pub(crate) async fn login(
    State(state): State<AppState>,
    ClientAddress(client): ClientAddress,
    JsonBody(request): JsonBody<LoginRequest>,
) -> ApiResult<Response> {
    let inner = &state.inner;
    let keys = LoginThrottle::keys(&request.username, client.as_deref());
    if let Err(wait) = inner.throttle.check(&keys, Instant::now()) {
        let mut error = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "too many failed logins; try again later",
        );
        error.retry_after = Some(wait.as_secs().max(1));
        return Err(error);
    }
    let auth = inner.auth.clone();
    let now = inner.engine.clock().now();
    let username = request.username.clone();
    let password = request.password;
    let result = tokio::task::spawn_blocking(move || auth.authenticate(&username, &password, now))
        .await
        .map_err(ApiError::internal)?;
    let user = match result {
        Ok(user) => user,
        Err(error @ (AuthError::InvalidCredentials | AuthError::AccountDisabled)) => {
            inner.throttle.record_failure(&keys, Instant::now());
            inner
                .counters
                .login_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            audit::record_best_effort(
                &state,
                &request.username,
                "auth.login_failed",
                None,
                client.clone().map(|address| format!("client {address}")),
            )
            .await;
            return Err(error.into());
        }
        Err(other) => return Err(other.into()),
    };
    inner.throttle.record_success(&user.username);
    let grant = inner
        .auth
        .create_session(&user, inner.config.sessions, client.as_deref(), now)?;
    audit::record_best_effort(
        &state,
        &user.username,
        "auth.login",
        None,
        client.map(|address| format!("client {address}")),
    )
    .await;
    let max_age = inner.config.sessions.max.as_secs();
    let secure = inner.config.secure_cookies;
    let body = json!({
        "token": grant.token,
        "csrf_token": grant.csrf_token,
        "expires_at": grant.expires_at,
        "user": {
            "username": user.username,
            "display_name": user.display_name,
            "role": user.role,
            "kind": "session",
            "permissions": user.role.permissions(),
        },
    });
    let mut response = Json(body).into_response();
    for value in [
        cookie_header(SESSION_COOKIE, &grant.token, max_age, true, secure),
        cookie_header(CSRF_COOKIE, &grant.csrf_token, max_age, false, secure),
    ]
    .into_iter()
    .flatten()
    {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    Ok(response)
}

/// `POST /auth/logout`
pub(crate) async fn logout(State(state): State<AppState>, caller: Caller) -> ApiResult<Response> {
    if let Some(token) = &caller.session_token {
        state.inner.auth.end_session(token)?;
    }
    audit::record_best_effort(&state, &caller.actor(), "auth.logout", None, None).await;
    let mut response = StatusCode::NO_CONTENT.into_response();
    clear_cookies(response.headers_mut(), state.inner.config.secure_cookies);
    Ok(response)
}

/// `GET /auth/me`
pub(crate) async fn me(caller: Caller) -> Json<serde_json::Value> {
    Json(principal_json(&caller.principal))
}

#[derive(Deserialize)]
pub(crate) struct PasswordChange {
    current_password: String,
    new_password: String,
}

/// `POST /auth/password`: a user changes their own password. Every session
/// of the user ends, including the current one.
pub(crate) async fn change_password(
    State(state): State<AppState>,
    caller: Caller,
    JsonBody(change): JsonBody<PasswordChange>,
) -> ApiResult<Response> {
    if caller.principal.user_id.is_none() {
        return Err(ApiError::forbidden("API tokens have no password"));
    }
    let auth = state.inner.auth.clone();
    let now = state.inner.engine.clock().now();
    let username = caller.principal.username.clone();
    tokio::task::spawn_blocking(move || {
        auth.authenticate(&username, &change.current_password, now)?;
        auth.set_password(&username, &change.new_password, now)
    })
    .await
    .map_err(ApiError::internal)??;
    audit::record_best_effort(&state, &caller.actor(), "user.password_changed", None, None).await;
    let mut response = StatusCode::NO_CONTENT.into_response();
    clear_cookies(response.headers_mut(), state.inner.config.secure_cookies);
    Ok(response)
}
