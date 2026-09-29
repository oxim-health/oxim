//! Request handlers, the router and the OpenAPI description.

pub(crate) mod admin;
pub(crate) mod auth;
pub(crate) mod channels;
pub(crate) mod events;
pub(crate) mod messages;
pub(crate) mod metrics;
pub(crate) mod tables;

use std::sync::LazyLock;

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use oxim_auth::Permission;
use serde_json::{Map, Value, json};

use crate::error::ApiError;
use crate::state::AppState;

/// Prefix of every API path.
pub const API_PREFIX: &str = "/api/v1";

/// The kind of request body an endpoint takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Body {
    None,
    Json,
    Yaml,
    Csv,
}

/// One API operation, as described in the OpenAPI document.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Endpoint {
    pub(crate) method: &'static str,
    /// Full path with `{name}` parameters.
    pub(crate) path: &'static str,
    pub(crate) summary: &'static str,
    /// `None` for public endpoints; `Some(None)` for any authenticated
    /// caller.
    pub(crate) access: Option<Option<Permission>>,
    pub(crate) body: Body,
    pub(crate) query: &'static [&'static str],
    pub(crate) status: u16,
}

const fn op(
    method: &'static str,
    path: &'static str,
    summary: &'static str,
    access: Option<Option<Permission>>,
) -> Endpoint {
    Endpoint {
        method,
        path,
        summary,
        access,
        body: Body::None,
        query: &[],
        status: 200,
    }
}

impl Endpoint {
    const fn body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    const fn query(mut self, query: &'static [&'static str]) -> Self {
        self.query = query;
        self
    }

    const fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
}

const ANY: Option<Option<Permission>> = Some(None);

const fn needs(permission: Permission) -> Option<Option<Permission>> {
    Some(Some(permission))
}

use Permission as P;

/// Every operation of the API. The router below must serve each of them; a
/// test checks that every entry answers (with `401` when unauthenticated).
pub(crate) const ENDPOINTS: &[Endpoint] = &[
    op(
        "post",
        "/api/v1/auth/login",
        "Log in with a user name and password",
        None,
    )
    .body(Body::Json),
    op(
        "post",
        "/api/v1/auth/logout",
        "End the current session",
        ANY,
    )
    .status(204),
    op("get", "/api/v1/auth/me", "The current user or token", ANY),
    op(
        "post",
        "/api/v1/auth/password",
        "Change the current user's password",
        ANY,
    )
    .body(Body::Json)
    .status(204),
    op(
        "get",
        "/api/v1/channels",
        "List channel files with state and queue depth",
        needs(P::ViewChannels),
    ),
    op(
        "get",
        "/api/v1/channels/{id}",
        "A channel's YAML (may hold credentials, so editors only)",
        needs(P::EditChannels),
    ),
    op(
        "put",
        "/api/v1/channels/{id}",
        "Validate and save a channel's YAML",
        needs(P::EditChannels),
    )
    .body(Body::Yaml),
    op(
        "delete",
        "/api/v1/channels/{id}",
        "Undeploy a channel and move its file to .deleted/",
        needs(P::EditChannels),
    )
    .status(204),
    op(
        "post",
        "/api/v1/channels/{id}/deploy",
        "Deploy a channel",
        needs(P::DeployChannels),
    )
    .status(204),
    op(
        "post",
        "/api/v1/channels/{id}/undeploy",
        "Undeploy a channel",
        needs(P::DeployChannels),
    )
    .status(204),
    op(
        "post",
        "/api/v1/channels/{id}/redeploy",
        "Redeploy a channel from its file",
        needs(P::DeployChannels),
    )
    .status(204),
    op(
        "get",
        "/api/v1/messages",
        "Search messages, newest first (keyset pagination)",
        needs(P::ViewMessages),
    )
    .query(&[
        "channel",
        "status",
        "destination_status",
        "from",
        "until",
        "before",
        "limit",
    ]),
    op(
        "get",
        "/api/v1/messages/{id}",
        "A message record and its stored contents",
        needs(P::ViewMessages),
    ),
    op(
        "get",
        "/api/v1/messages/{id}/content",
        "One stage's content, masked unless permitted; audited",
        needs(P::ViewMessages),
    )
    .query(&["stage", "destination"]),
    op(
        "post",
        "/api/v1/messages/{id}/content/unmasked",
        "Break-glass unmasked content with a reason; audited",
        needs(P::ViewMessages),
    )
    .body(Body::Json),
    op(
        "post",
        "/api/v1/messages/{id}/reprocess",
        "Process a message again",
        needs(P::RepairMessages),
    ),
    op(
        "post",
        "/api/v1/messages/{id}/requeue",
        "Retry a failed delivery now",
        needs(P::RepairMessages),
    )
    .body(Body::Json)
    .status(204),
    op(
        "post",
        "/api/v1/messages/{id}/erase",
        "Delete a message and its contents; audited",
        needs(P::EraseMessages),
    )
    .body(Body::Json)
    .status(204),
    op(
        "get",
        "/api/v1/tables",
        "List code tables",
        needs(P::ViewTables),
    ),
    op(
        "get",
        "/api/v1/tables/{name}",
        "A code table's CSV",
        needs(P::ViewTables),
    ),
    op(
        "put",
        "/api/v1/tables/{name}",
        "Validate and save a code table",
        needs(P::EditTables),
    )
    .body(Body::Csv),
    op("get", "/api/v1/users", "List users", needs(P::ManageUsers)),
    op(
        "post",
        "/api/v1/users",
        "Create a user",
        needs(P::ManageUsers),
    )
    .body(Body::Json)
    .status(201),
    op(
        "patch",
        "/api/v1/users/{username}",
        "Change a user's name, role or disabled flag",
        needs(P::ManageUsers),
    )
    .body(Body::Json),
    op(
        "post",
        "/api/v1/users/{username}/password",
        "Set a user's password",
        needs(P::ManageUsers),
    )
    .body(Body::Json)
    .status(204),
    op(
        "delete",
        "/api/v1/users/{username}/sessions",
        "End every session of a user",
        needs(P::ManageUsers),
    ),
    op(
        "get",
        "/api/v1/tokens",
        "List API tokens",
        needs(P::ManageTokens),
    ),
    op(
        "post",
        "/api/v1/tokens",
        "Create an API token (shown once)",
        needs(P::ManageTokens),
    )
    .body(Body::Json)
    .status(201),
    op(
        "delete",
        "/api/v1/tokens/{id}",
        "Revoke an API token",
        needs(P::ManageTokens),
    )
    .status(204),
    op(
        "get",
        "/api/v1/audit",
        "Query the audit trail",
        needs(P::ViewAudit),
    )
    .query(&["message", "action", "actor", "limit"]),
    op(
        "get",
        "/api/v1/system",
        "Version, uptime and component types",
        needs(P::ViewSystem),
    ),
    op(
        "get",
        "/api/v1/events",
        "Server-sent `stats` events for the dashboard",
        needs(P::ViewDashboard),
    ),
    op("get", "/api/v1/openapi.json", "This document", None),
    op(
        "get",
        "/metrics",
        "Prometheus metrics",
        needs(P::ViewSystem),
    ),
];

/// The API routes below [`API_PREFIX`], except the event stream, which is
/// returned separately so it escapes the request timeout.
pub(crate) fn api() -> (Router<AppState>, Router<AppState>) {
    let timed = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/password", post(auth::change_password))
        .route("/channels", get(channels::list))
        .route(
            "/channels/{id}",
            get(channels::get)
                .put(channels::put)
                .delete(channels::delete),
        )
        .route("/channels/{id}/deploy", post(channels::deploy))
        .route("/channels/{id}/undeploy", post(channels::undeploy))
        .route("/channels/{id}/redeploy", post(channels::redeploy))
        .route("/messages", get(messages::list))
        .route("/messages/{id}", get(messages::get))
        .route("/messages/{id}/content", get(messages::content))
        .route(
            "/messages/{id}/content/unmasked",
            post(messages::break_glass),
        )
        .route("/messages/{id}/reprocess", post(messages::reprocess))
        .route("/messages/{id}/requeue", post(messages::requeue))
        .route("/messages/{id}/erase", post(messages::erase))
        .route("/tables", get(tables::list))
        .route("/tables/{name}", get(tables::get).put(tables::put))
        .route("/users", get(admin::users).post(admin::create_user))
        .route("/users/{username}", patch(admin::update_user))
        .route("/users/{username}/password", post(admin::reset_password))
        .route("/users/{username}/sessions", delete(admin::revoke_sessions))
        .route("/tokens", get(admin::tokens).post(admin::create_token))
        .route("/tokens/{id}", delete(admin::revoke_token))
        .route("/audit", get(admin::audit_trail))
        .route("/system", get(admin::system))
        .route("/openapi.json", get(openapi));
    let streaming = Router::new().route("/events", get(events::stream));
    (timed, streaming)
}

/// Unknown API paths.
pub(crate) async fn not_found() -> ApiError {
    ApiError::not_found("no such API endpoint")
}

/// Known paths with the wrong method.
pub(crate) async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "this endpoint does not support the method",
    )
}

/// `GET /api/v1/openapi.json`
pub(crate) async fn openapi() -> Json<Value> {
    Json(OPENAPI.clone())
}

static OPENAPI: LazyLock<Value> = LazyLock::new(openapi_document);

fn permission_name(permission: Permission) -> String {
    serde_json::to_value(permission)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn operation_id(endpoint: &Endpoint) -> String {
    let mut id = endpoint.method.to_owned();
    for part in endpoint.path.trim_start_matches(API_PREFIX).split('/') {
        let part = part.trim_matches(|c| c == '{' || c == '}');
        for word in part.split(|c: char| !c.is_ascii_alphanumeric()) {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                id.push(first.to_ascii_uppercase());
                id.extend(chars);
            }
        }
    }
    id
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } },
    })
}

fn operation(endpoint: &Endpoint) -> Value {
    let mut parameters = Vec::new();
    for segment in endpoint.path.split('/') {
        if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            parameters.push(json!({
                "name": name, "in": "path", "required": true, "schema": { "type": "string" },
            }));
        }
    }
    for name in endpoint.query {
        parameters.push(json!({
            "name": name, "in": "query", "required": *name == "stage", "schema": { "type": "string" },
        }));
    }
    let mut responses = Map::new();
    let success = match endpoint.status {
        201 => "Created",
        204 => "Done",
        _ => "OK",
    };
    let content_type = match endpoint.path {
        "/metrics" => Some("text/plain"),
        "/api/v1/events" => Some("text/event-stream"),
        _ if endpoint.status == 204 => None,
        _ => Some("application/json"),
    };
    let mut ok = json!({ "description": success });
    if let Some(kind) = content_type {
        ok["content"] = json!({ kind: {} });
    }
    responses.insert(endpoint.status.to_string(), ok);
    responses.insert("400".into(), error_response("Invalid request"));
    let mut description = String::new();
    match endpoint.access {
        None => {
            description.push_str("Public.");
        }
        Some(permission) => {
            responses.insert("401".into(), error_response("Not authenticated"));
            responses.insert(
                "403".into(),
                error_response("Not permitted (or missing CSRF header)"),
            );
            match permission {
                Some(permission) => description.push_str(&format!(
                    "Requires the `{}` permission.",
                    permission_name(permission)
                )),
                None => description.push_str("Any authenticated caller."),
            }
        }
    }
    responses.insert("default".into(), error_response("Error"));
    let mut operation = json!({
        "operationId": operation_id(endpoint),
        "summary": endpoint.summary,
        "description": description,
        "parameters": parameters,
        "responses": responses,
    });
    if endpoint.access.is_none() {
        operation["security"] = json!([]);
    }
    let body = match endpoint.body {
        Body::None => None,
        Body::Json => Some(("application/json", json!({ "type": "object" }))),
        Body::Yaml => Some(("application/yaml", json!({ "type": "string" }))),
        Body::Csv => Some(("text/csv", json!({ "type": "string" }))),
    };
    if let Some((kind, schema)) = body {
        operation["requestBody"] =
            json!({ "required": true, "content": { kind: { "schema": schema } } });
    }
    operation
}

/// The OpenAPI 3.1 description of [`ENDPOINTS`].
pub(crate) fn openapi_document() -> Value {
    let mut paths = Map::new();
    for endpoint in ENDPOINTS {
        let entry = paths
            .entry(endpoint.path.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        entry[endpoint.method] = operation(endpoint);
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "OXIM API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Management API of the OXIM healthcare integration engine. \
                Authenticate with `POST /api/v1/auth/login` (session cookie plus \
                `X-CSRF-Token` header for changes, or the returned token as a bearer \
                token) or with an API token as a bearer token. Errors are \
                `{\"error\": {\"code\", \"message\"}}`.",
        },
        "servers": [{ "url": "/" }],
        "security": [{ "bearer": [] }, { "session": [] }],
        "components": {
            "securitySchemes": {
                "bearer": { "type": "http", "scheme": "bearer" },
                "session": { "type": "apiKey", "in": "cookie", "name": "oxim_session" },
            },
            "schemas": {
                "Error": {
                    "type": "object",
                    "required": ["error"],
                    "properties": {
                        "error": {
                            "type": "object",
                            "required": ["code", "message"],
                            "properties": {
                                "code": { "type": "string" },
                                "message": { "type": "string" },
                            },
                        },
                    },
                },
            },
        },
        "paths": paths,
    })
}
