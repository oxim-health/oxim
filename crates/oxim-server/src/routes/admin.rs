//! Users, API tokens, the audit trail and system information.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use oxim_auth::{NewUser, Permission, Role, User, UserUpdate};
use oxim_model::{MessageId, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit;
use crate::caller::Caller;
use crate::error::{ApiError, ApiResult};
use crate::extract::{JsonBody, PathParam, QueryParams};
use crate::state::AppState;

fn user_json(user: &User) -> Value {
    json!({
        "username": user.username,
        "display_name": user.display_name,
        "role": user.role,
        "disabled": user.disabled,
        "created_at": user.created_at,
        "updated_at": user.updated_at,
        "last_login_at": user.last_login_at,
    })
}

fn role(text: &str) -> ApiResult<Role> {
    text.parse()
        .map_err(|e: oxim_auth::UnknownRole| ApiError::bad_request(e.to_string()))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, oxim_auth::AuthError> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::from)
}

/// `GET /users`
pub(crate) async fn users(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    caller.require(Permission::ManageUsers)?;
    let users = state.inner.auth.users()?;
    Ok(Json(
        json!({ "users": users.iter().map(user_json).collect::<Vec<_>>() }),
    ))
}

/// Body of `POST /users`.
#[derive(Deserialize)]
pub(crate) struct CreateUser {
    username: String,
    display_name: Option<String>,
    password: String,
    role: String,
}

/// `POST /users`
pub(crate) async fn create_user(
    State(state): State<AppState>,
    caller: Caller,
    JsonBody(request): JsonBody<CreateUser>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::ManageUsers)?;
    let role = role(&request.role)?;
    let auth = state.inner.auth.clone();
    let now = state.inner.engine.clock().now();
    let user = blocking(move || {
        auth.create_user(
            &NewUser {
                username: &request.username,
                display_name: request.display_name.as_deref().unwrap_or(&request.username),
                password: &request.password,
                role,
            },
            now,
        )
    })
    .await?;
    audit::record(
        &state,
        &caller.actor(),
        "user.created",
        None,
        None,
        Some(format!("{} role={}", user.username, user.role)),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(user_json(&user))))
}

/// Body of `PATCH /users/{username}`.
#[derive(Deserialize)]
pub(crate) struct ChangeUser {
    display_name: Option<String>,
    role: Option<String>,
    disabled: Option<bool>,
}

/// Whether the change leaves at least one enabled administrator.
fn keeps_an_admin(
    users: &[User],
    username: &str,
    role: Option<Role>,
    disabled: Option<bool>,
) -> bool {
    users.iter().any(|user| {
        let target = user.username.eq_ignore_ascii_case(username);
        let role = if target {
            role.unwrap_or(user.role)
        } else {
            user.role
        };
        let disabled = if target {
            disabled.unwrap_or(user.disabled)
        } else {
            user.disabled
        };
        role == Role::Admin && !disabled
    })
}

/// `PATCH /users/{username}`: display name, role or disabled flag.
/// Disabling or demoting a user ends their sessions. The last enabled
/// administrator cannot be disabled or demoted.
pub(crate) async fn update_user(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(username): PathParam<String>,
    JsonBody(request): JsonBody<ChangeUser>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ManageUsers)?;
    let role = request.role.as_deref().map(role).transpose()?;
    let users = state.inner.auth.users()?;
    if !keeps_an_admin(&users, &username, role, request.disabled) {
        return Err(ApiError::conflict(
            "the last enabled administrator cannot be disabled or demoted",
        ));
    }
    let now = state.inner.engine.clock().now();
    let update = UserUpdate {
        display_name: request.display_name,
        role,
        disabled: request.disabled,
    };
    let user = state.inner.auth.update_user(&username, &update, now)?;
    let mut changes = Vec::new();
    if let Some(role) = update.role {
        changes.push(format!("role={role}"));
    }
    if let Some(disabled) = update.disabled {
        changes.push(format!("disabled={disabled}"));
    }
    if update.display_name.is_some() {
        changes.push("display_name".to_owned());
    }
    audit::record(
        &state,
        &caller.actor(),
        "user.updated",
        None,
        None,
        Some(format!("{} {}", user.username, changes.join(" "))),
    )
    .await?;
    Ok(Json(user_json(&user)))
}

/// Body of `POST /users/{username}/password`.
#[derive(Deserialize)]
pub(crate) struct ResetPassword {
    password: String,
}

/// `POST /users/{username}/password`: sets a new password and ends the
/// user's sessions.
pub(crate) async fn reset_password(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(username): PathParam<String>,
    JsonBody(request): JsonBody<ResetPassword>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::ManageUsers)?;
    let auth = state.inner.auth.clone();
    let now = state.inner.engine.clock().now();
    let name = username.clone();
    blocking(move || auth.set_password(&name, &request.password, now)).await?;
    audit::record(
        &state,
        &caller.actor(),
        "user.password_reset",
        None,
        None,
        Some(username),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /users/{username}/sessions`: logs the user out everywhere.
pub(crate) async fn revoke_sessions(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(username): PathParam<String>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ManageUsers)?;
    if state.inner.auth.user(&username)?.is_none() {
        return Err(ApiError::not_found(format!("user {username} not found")));
    }
    let ended = state.inner.auth.end_user_sessions(&username)?;
    audit::record(
        &state,
        &caller.actor(),
        "user.sessions_revoked",
        None,
        None,
        Some(format!("{username} sessions={ended}")),
    )
    .await?;
    Ok(Json(json!({ "ended": ended })))
}

/// `GET /tokens`
pub(crate) async fn tokens(
    State(state): State<AppState>,
    caller: Caller,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ManageTokens)?;
    let tokens: Vec<Value> = state
        .inner
        .auth
        .api_tokens()?
        .into_iter()
        .map(|token| {
            json!({
                "id": token.id,
                "name": token.name,
                "role": token.role,
                "created_by": token.created_by,
                "created_at": token.created_at,
                "expires_at": token.expires_at,
                "last_used_at": token.last_used_at,
                "revoked": token.revoked,
            })
        })
        .collect();
    Ok(Json(json!({ "tokens": tokens })))
}

/// Body of `POST /tokens`.
#[derive(Deserialize)]
pub(crate) struct CreateToken {
    name: String,
    role: String,
    expires_at: Option<Timestamp>,
}

/// `POST /tokens`: the token is in the response and never shown again.
pub(crate) async fn create_token(
    State(state): State<AppState>,
    caller: Caller,
    JsonBody(request): JsonBody<CreateToken>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::ManageTokens)?;
    let role = role(&request.role)?;
    if !caller.principal.role.covers(role) {
        return Err(ApiError::forbidden(
            "a token cannot have more permissions than its creator",
        ));
    }
    let now = state.inner.engine.clock().now();
    if request.expires_at.is_some_and(|at| at <= now) {
        return Err(ApiError::bad_request("expires_at is in the past"));
    }
    let (secret, token) = state.inner.auth.create_api_token(
        &request.name,
        role,
        &caller.actor(),
        request.expires_at,
        now,
    )?;
    audit::record(
        &state,
        &caller.actor(),
        "token.created",
        None,
        None,
        Some(format!(
            "id={} name={} role={}",
            token.id, token.name, token.role
        )),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": token.id,
            "name": token.name,
            "role": token.role,
            "expires_at": token.expires_at,
            "token": secret,
        })),
    ))
}

/// `DELETE /tokens/{id}`
pub(crate) async fn revoke_token(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<i64>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::ManageTokens)?;
    state.inner.auth.revoke_api_token(id)?;
    audit::record(
        &state,
        &caller.actor(),
        "token.revoked",
        None,
        None,
        Some(format!("id={id}")),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Query parameters of `GET /audit`.
#[derive(Deserialize)]
pub(crate) struct AuditParams {
    message: Option<String>,
    action: Option<String>,
    actor: Option<String>,
    limit: Option<usize>,
}

/// `GET /audit`: newest first, optionally for one message, action prefix
/// or actor.
pub(crate) async fn audit_trail(
    State(state): State<AppState>,
    caller: Caller,
    QueryParams(params): QueryParams<AuditParams>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewAudit)?;
    let limit = params.limit.unwrap_or(200).clamp(1, 5000);
    let message = params
        .message
        .as_deref()
        .filter(|text| !text.is_empty())
        .map(|text| {
            text.parse::<MessageId>()
                .map_err(|_| ApiError::bad_request(format!("invalid message id {text:?}")))
        })
        .transpose()?;
    let filtered = params.action.is_some() || params.actor.is_some();
    let fetch = if filtered { 10_000 } else { limit };
    let events = state
        .inner
        .engine
        .store()
        .run(move |store| store.audit_trail(message, fetch))
        .await?;
    let events: Vec<Value> = events
        .into_iter()
        .filter(|event| {
            params
                .action
                .as_deref()
                .is_none_or(|prefix| event.action.starts_with(prefix))
                && params
                    .actor
                    .as_deref()
                    .is_none_or(|actor| event.actor == actor)
        })
        .take(limit)
        .map(|event| {
            json!({
                "at": event.at,
                "action": event.action,
                "actor": event.actor,
                "message_id": event.message_id,
                "channel": event.channel,
                "detail": event.detail,
            })
        })
        .collect();
    Ok(Json(json!({ "events": events })))
}

/// `GET /system`
pub(crate) async fn system(
    State(state): State<AppState>,
    caller: Caller,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewSystem)?;
    let inner = &state.inner;
    let deployed = inner.engine.deployed().await;
    let types: serde_json::Map<String, Value> = inner
        .engine
        .registry()
        .type_names()
        .into_iter()
        .map(|(kind, names)| (kind.to_owned(), json!(names)))
        .collect();
    Ok(Json(json!({
        "name": "oxim",
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": inner.started_at,
        "uptime_seconds": inner.started.elapsed().as_secs(),
        "now": inner.engine.clock().now(),
        "deployed_channels": deployed,
        "tls": inner.config.tls.is_some(),
        "session_idle_seconds": inner.config.sessions.idle.as_secs(),
        "session_max_seconds": inner.config.sessions.max.as_secs(),
        "component_types": types,
        "maintenance": super::ops::maintenance_json(&state),
    })))
}
