//! Server-sent events for the dashboard.
//!
//! The stream polls the store once a second and sends a `stats` event when
//! the per-channel counts or the deployed channels change. Streams are
//! limited in number and length (clients reconnect and authenticate again),
//! and end when the server shuts down.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use oxim_auth::Permission;
use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;

use crate::caller::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// How often the store is polled.
const POLL: Duration = Duration::from_secs(1);
/// How long one stream lasts before the client must reconnect.
const MAX_STREAM: Duration = Duration::from_secs(300);

async fn snapshot(state: &AppState) -> Option<Value> {
    let counts = state
        .inner
        .engine
        .store()
        .run(|store| store.status_counts())
        .await
        .ok()?;
    let deployed = state.inner.engine.deployed().await;
    let messages: Vec<Value> = counts
        .messages
        .iter()
        .map(|(channel, status, count)| json!({ "channel": channel, "status": status, "count": count }))
        .collect();
    let deliveries: Vec<Value> = counts
        .deliveries
        .iter()
        .map(|(channel, destination, status, count)| {
            json!({ "channel": channel, "destination": destination, "status": status, "count": count })
        })
        .collect();
    Some(json!({ "deployed": deployed, "messages": messages, "deliveries": deliveries }))
}

struct Cursor {
    state: AppState,
    last: Option<Value>,
    deadline: tokio::time::Instant,
    first: bool,
    _permit: OwnedSemaphorePermit,
}

fn events(
    state: AppState,
    permit: OwnedSemaphorePermit,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let initial = Cursor {
        deadline: tokio::time::Instant::now() + MAX_STREAM,
        state,
        last: None,
        first: true,
        _permit: permit,
    };
    stream::unfold(initial, |mut current| async move {
        loop {
            if !current.first {
                let shutdown = current.state.inner.shutdown.clone();
                tokio::select! {
                    () = shutdown.cancelled() => return None,
                    () = tokio::time::sleep_until(current.deadline) => return None,
                    () = tokio::time::sleep(POLL) => {}
                }
            }
            current.first = false;
            let Some(value) = snapshot(&current.state).await else {
                continue;
            };
            if current.last.as_ref() == Some(&value) {
                continue;
            }
            let event = Event::default().event("stats").data(value.to_string());
            current.last = Some(value);
            return Some((Ok(event), current));
        }
    })
}

/// `GET /events`
pub(crate) async fn stream(State(state): State<AppState>, caller: Caller) -> ApiResult<Response> {
    caller.require(Permission::ViewDashboard)?;
    let permit = state
        .inner
        .event_clients
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "too_many_streams",
                "too many open event streams; try again later",
            )
        })?;
    Ok(Sse::new(events(state, permit))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}
