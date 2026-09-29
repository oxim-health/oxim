//! API errors.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use oxim_auth::AuthError;
use oxim_core::EngineError;
use oxim_store::StoreError;
use serde_json::json;

/// An error returned to API clients as
/// `{ "error": { "code": ..., "message": ... } }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// A stable machine-readable code such as `not_found`.
    pub code: &'static str,
    /// A human-readable explanation.
    pub message: String,
    /// Seconds to wait before retrying, for `429` responses.
    pub retry_after: Option<u64>,
}

impl ApiError {
    /// Creates an error.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    /// `400 bad_request`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// `401 unauthenticated`.
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "log in or send a valid bearer token",
        )
    }

    /// `403 forbidden`.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    /// `404 not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// `409 conflict`.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    /// `500 internal`, logging the detail and hiding it from the client.
    pub fn internal(detail: impl std::fmt::Display) -> Self {
        tracing::error!(error = %detail, "API request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "the request failed; see the server log",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response();
        if let Some(seconds) = self.retry_after
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

impl From<EngineError> for ApiError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Config(message) => {
                Self::new(StatusCode::BAD_REQUEST, "invalid_configuration", message)
            }
            EngineError::UnknownType { .. } => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_configuration",
                error.to_string(),
            ),
            EngineError::AlreadyDeployed(_) | EngineError::NotDeployed(_) => {
                Self::conflict(error.to_string())
            }
            EngineError::Store(store) => store.into(),
            other => Self::internal(other),
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::MessageNotFound(_) | StoreError::DeliveryNotFound(..) => {
                Self::not_found(error.to_string())
            }
            StoreError::InvalidState(message) => Self::conflict(message),
            other => Self::internal(other),
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::InvalidCredentials | AuthError::AccountDisabled => Self::new(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "invalid user name or password",
            ),
            AuthError::InvalidToken => Self::unauthenticated(),
            AuthError::Policy(message) => Self::bad_request(message),
            AuthError::InvalidUsername => Self::bad_request(error.to_string()),
            AuthError::UserExists(_) => Self::conflict(error.to_string()),
            AuthError::UserNotFound(_) | AuthError::TokenNotFound(_) => {
                Self::not_found(error.to_string())
            }
            other => Self::internal(other),
        }
    }
}

/// Result type of handlers.
pub type ApiResult<T> = Result<T, ApiError>;
