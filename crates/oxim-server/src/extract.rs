//! Extractors whose rejections use the API's JSON error format.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection, StringRejection};
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::request::Parts;
use axum::{Json, RequestExt as _};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

fn invalid(status: axum::http::StatusCode, message: String) -> ApiError {
    let code = if status == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
        "payload_too_large"
    } else {
        "invalid_request"
    };
    ApiError::new(status, code, message)
}

/// A JSON request body.
pub(crate) struct JsonBody<T>(pub(crate) T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for JsonBody<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => {
                let rejection: JsonRejection = rejection;
                Err(invalid(rejection.status(), rejection.body_text()))
            }
        }
    }
}

/// A UTF-8 text request body, such as channel YAML or table CSV.
pub(crate) struct TextBody(pub(crate) String);

impl<S: Send + Sync> FromRequest<S> for TextBody {
    type Rejection = ApiError;

    async fn from_request(request: Request, _: &S) -> Result<Self, ApiError> {
        match request.extract::<String, _>().await {
            Ok(text) => Ok(Self(text)),
            Err(rejection) => {
                let rejection: StringRejection = rejection;
                Err(invalid(rejection.status(), rejection.body_text()))
            }
        }
    }
}

/// Query parameters.
pub(crate) struct QueryParams<T>(pub(crate) T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for QueryParams<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => {
                let rejection: QueryRejection = rejection;
                Err(invalid(rejection.status(), rejection.body_text()))
            }
        }
    }
}

/// Path parameters.
pub(crate) struct PathParam<T>(pub(crate) T);

impl<T: DeserializeOwned + Send, S: Send + Sync> FromRequestParts<S> for PathParam<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Self(value)),
            Err(rejection) => {
                let rejection: PathRejection = rejection;
                Err(invalid(rejection.status(), rejection.body_text()))
            }
        }
    }
}
