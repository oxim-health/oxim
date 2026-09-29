//! Stored messages: listing, contents with masking, and repairs.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::Engine as _;
use oxim_auth::{Permission, PrincipalKind};
use oxim_model::{ConnectorId, DestinationStatus, MessageId, MessageStatus, Timestamp};
use oxim_store::{Content, MessageQuery, MessageRecord, Stage};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit;
use crate::caller::Caller;
use crate::error::{ApiError, ApiResult};
use crate::extract::{JsonBody, PathParam, QueryParams};
use crate::mask::{MASK, Masked, mask};
use crate::state::AppState;

/// Shortest accepted reason for break-glass access and erasure.
pub(crate) const MIN_REASON_LEN: usize = 8;

fn parse<T: std::str::FromStr>(name: &str, value: Option<&str>) -> ApiResult<Option<T>> {
    value
        .filter(|text| !text.is_empty())
        .map(|text| {
            text.parse()
                .map_err(|_| ApiError::bad_request(format!("invalid {name} {text:?}")))
        })
        .transpose()
}

fn message_id(text: &str) -> ApiResult<MessageId> {
    text.parse()
        .map_err(|_| ApiError::bad_request(format!("invalid message id {text:?}")))
}

fn reason(text: Option<&str>) -> ApiResult<String> {
    let reason = text.unwrap_or_default().trim();
    if reason.chars().count() < MIN_REASON_LEN {
        return Err(ApiError::bad_request(format!(
            "a reason of at least {MIN_REASON_LEN} characters is required"
        )));
    }
    Ok(reason.chars().take(500).collect())
}

/// A message record as JSON. Metadata values may identify a patient (file
/// names, for example), so they are masked for callers who see masked
/// content.
fn record_json(record: &MessageRecord, unmasked: bool) -> Value {
    let metadata: serde_json::Map<String, Value> = record
        .metadata
        .iter()
        .map(|(key, value)| {
            let value = if unmasked { value.as_str() } else { MASK };
            (key.clone(), Value::String(value.to_owned()))
        })
        .collect();
    let destinations: Vec<Value> = record
        .destinations
        .iter()
        .map(|state| {
            json!({
                "destination": state.destination,
                "status": state.status,
                "attempts": state.attempts,
                "last_error": state.last_error,
                "next_attempt_at": state.next_attempt_at,
                "updated_at": state.updated_at,
            })
        })
        .collect();
    json!({
        "id": record.id,
        "channel": record.channel,
        "connector": record.connector,
        "received_at": record.received_at,
        "data_type": record.data_type.as_str(),
        "status": record.status,
        "peer": record.peer,
        "device": record.device,
        "correlation_id": record.correlation_id,
        "metadata": metadata,
        "error": record.error,
        "destinations": destinations,
    })
}

/// Query parameters of `GET /messages`.
#[derive(Deserialize, Default)]
pub(crate) struct ListParams {
    channel: Option<String>,
    status: Option<String>,
    destination_status: Option<String>,
    from: Option<String>,
    until: Option<String>,
    before: Option<String>,
    limit: Option<usize>,
}

/// `GET /messages`
pub(crate) async fn list(
    State(state): State<AppState>,
    caller: Caller,
    QueryParams(params): QueryParams<ListParams>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewMessages)?;
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let query = MessageQuery {
        channel: parse("channel", params.channel.as_deref())?,
        status: parse::<MessageStatus>("status", params.status.as_deref())?,
        destination_status: parse::<DestinationStatus>(
            "destination_status",
            params.destination_status.as_deref(),
        )?,
        from: parse::<Timestamp>("from", params.from.as_deref())?,
        until: parse::<Timestamp>("until", params.until.as_deref())?,
        before: parse::<MessageId>("before", params.before.as_deref())?,
        limit,
    };
    let records = state
        .inner
        .engine
        .store()
        .run(move |store| store.list_messages(&query))
        .await?;
    let unmasked = caller.principal.can(Permission::ViewUnmasked);
    let next = (records.len() == limit)
        .then(|| records.last().map(|record| record.id))
        .flatten();
    Ok(Json(json!({
        "messages": records.iter().map(|record| record_json(record, unmasked)).collect::<Vec<_>>(),
        "next_before": next,
    })))
}

async fn load_record(state: &AppState, id: MessageId) -> ApiResult<MessageRecord> {
    state
        .inner
        .engine
        .store()
        .run(move |store| store.message(id))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("message {id} not found")))
}

/// `GET /messages/{id}`: the record and which contents are stored.
pub(crate) async fn get(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewMessages)?;
    let id = message_id(&id)?;
    let record = load_record(&state, id).await?;
    let destinations: Vec<ConnectorId> = record
        .destinations
        .iter()
        .map(|state| state.destination.clone())
        .collect();
    let available = state
        .inner
        .engine
        .store()
        .run(move |store| {
            let mut found = Vec::new();
            for stage in [
                Stage::Raw,
                Stage::Normalized,
                Stage::Transformed,
                Stage::Reply,
            ] {
                if let Some(content) = store.content(id, stage, None)? {
                    found.push(content_info(&content));
                }
            }
            for destination in &destinations {
                for stage in [Stage::Encoded, Stage::Response] {
                    if let Some(content) = store.content(id, stage, Some(destination))? {
                        found.push(content_info(&content));
                    }
                }
            }
            Ok(found)
        })
        .await?;
    let mut body = record_json(&record, caller.principal.can(Permission::ViewUnmasked));
    body["contents"] = Value::Array(available);
    Ok(Json(body))
}

fn content_info(content: &Content) -> Value {
    json!({
        "stage": content.stage.as_str(),
        "destination": content.destination,
        "data_type": content.data_type.map(|kind| kind.as_str()),
        "size": content.data.len(),
    })
}

/// Which content to show.
#[derive(Deserialize)]
pub(crate) struct ContentParams {
    stage: String,
    destination: Option<String>,
}

async fn load_content(
    state: &AppState,
    id: MessageId,
    params: &ContentParams,
) -> ApiResult<Content> {
    let stage: Stage = params
        .stage
        .parse()
        .map_err(|()| ApiError::bad_request(format!("invalid stage {:?}", params.stage)))?;
    let destination = match (stage.is_per_destination(), params.destination.as_deref()) {
        (true, Some(name)) => {
            Some(ConnectorId::new(name).map_err(|e| ApiError::bad_request(e.to_string()))?)
        }
        (true, None) => {
            return Err(ApiError::bad_request(format!(
                "the {stage} stage needs a destination"
            )));
        }
        (false, _) => None,
    };
    state
        .inner
        .engine
        .store()
        .run(move |store| store.content(id, stage, destination.as_ref()))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("message {id} has no {stage} content")))
}

fn content_body(content: &Content, data: Option<&[u8]>, masked: bool) -> Value {
    let (encoding, text) = match data {
        None => (Value::Null, Value::Null),
        Some(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => (json!("utf8"), json!(text)),
            Err(_) => (
                json!("base64"),
                json!(base64::engine::general_purpose::STANDARD.encode(bytes)),
            ),
        },
    };
    json!({
        "stage": content.stage.as_str(),
        "destination": content.destination,
        "data_type": content.data_type.map(|kind| kind.as_str()),
        "masked": masked,
        "withheld": data.is_none(),
        "encoding": encoding,
        "data": text,
    })
}

fn describe(content: &Content) -> String {
    match &content.destination {
        Some(destination) => format!("stage={} destination={destination}", content.stage),
        None => format!("stage={}", content.stage),
    }
}

/// `GET /messages/{id}/content?stage=&destination=`: callers without
/// unmasked access get patient-identifying values masked. Every view is
/// audited, and the content is only returned once the audit event is
/// stored.
pub(crate) async fn content(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
    QueryParams(params): QueryParams<ContentParams>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewMessages)?;
    let id = message_id(&id)?;
    let record = load_record(&state, id).await?;
    let content = load_content(&state, id, &params).await?;
    let unmasked = caller.principal.can(Permission::ViewUnmasked);
    let masked = if unmasked {
        None
    } else {
        Some(mask(content.data_type, &content.data))
    };
    let view = if unmasked { "unmasked" } else { "masked" };
    audit::record(
        &state,
        &caller.actor(),
        "message.content_viewed",
        Some(id),
        Some(record.channel),
        Some(format!("{} {view}", describe(&content))),
    )
    .await?;
    let body = match &masked {
        None => content_body(&content, Some(&content.data), false),
        Some(Masked::Content(bytes)) => content_body(&content, Some(bytes), true),
        Some(Masked::Withheld) => content_body(&content, None, true),
    };
    Ok(Json(body))
}

/// Body of a break-glass request.
#[derive(Deserialize)]
pub(crate) struct BreakGlass {
    stage: String,
    destination: Option<String>,
    reason: Option<String>,
}

/// `POST /messages/{id}/content/unmasked`: break-glass access to unmasked
/// content for a logged-in user who normally sees it masked. A reason is
/// mandatory and the access is always audited. API tokens cannot break the
/// glass; they need a role with unmasked access.
pub(crate) async fn break_glass(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
    JsonBody(request): JsonBody<BreakGlass>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewMessages)?;
    if caller.principal.kind != PrincipalKind::Session {
        return Err(ApiError::forbidden(
            "break-glass access is only available to logged-in users",
        ));
    }
    let reason = reason(request.reason.as_deref())?;
    let id = message_id(&id)?;
    let record = load_record(&state, id).await?;
    let params = ContentParams {
        stage: request.stage,
        destination: request.destination,
    };
    let content = load_content(&state, id, &params).await?;
    audit::record(
        &state,
        &caller.actor(),
        "message.break_glass",
        Some(id),
        Some(record.channel),
        Some(format!("{} reason: {reason}", describe(&content))),
    )
    .await?;
    tracing::warn!(user = %caller.actor(), message = %id, "break-glass access to unmasked content");
    Ok(Json(content_body(&content, Some(&content.data), false)))
}

/// `POST /messages/{id}/reprocess`: discards everything derived from the
/// message and runs it through its channel again.
pub(crate) async fn reprocess(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::RepairMessages)?;
    let id = message_id(&id)?;
    let record = load_record(&state, id).await?;
    audit::record(
        &state,
        &caller.actor(),
        "message.reprocessed",
        Some(id),
        Some(record.channel.clone()),
        None,
    )
    .await?;
    state
        .inner
        .engine
        .store()
        .run(move |store| store.reprocess(id))
        .await?;
    // A channel that is not deployed picks the message up when it is.
    let scheduled = state
        .inner
        .engine
        .process_stored(&record.channel, id)
        .await
        .is_ok();
    Ok(Json(json!({ "id": id, "scheduled": scheduled })))
}

/// Body of a requeue request.
#[derive(Deserialize)]
pub(crate) struct Requeue {
    destination: String,
}

/// `POST /messages/{id}/requeue`: puts a failed or retrying delivery back
/// in its queue for an immediate attempt.
pub(crate) async fn requeue(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
    JsonBody(request): JsonBody<Requeue>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::RepairMessages)?;
    let id = message_id(&id)?;
    let destination =
        ConnectorId::new(request.destination).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let record = load_record(&state, id).await?;
    let now = state.inner.engine.clock().now();
    let target = destination.clone();
    state
        .inner
        .engine
        .store()
        .run(move |store| store.requeue(id, &target, now))
        .await?;
    audit::record(
        &state,
        &caller.actor(),
        "message.requeued",
        Some(id),
        Some(record.channel.clone()),
        Some(format!("destination={destination}")),
    )
    .await?;
    let _ = state
        .inner
        .engine
        .wake_destination(&record.channel, &destination)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Body of an erasure request.
#[derive(Deserialize)]
pub(crate) struct Erase {
    reason: Option<String>,
}

/// `POST /messages/{id}/erase`: deletes a message with all its contents,
/// for example to honor an erasure request. The audit event (with the
/// mandatory reason) is written first and survives the erasure.
pub(crate) async fn erase(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
    JsonBody(request): JsonBody<Erase>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::EraseMessages)?;
    let reason = reason(request.reason.as_deref())?;
    let id = message_id(&id)?;
    let record = load_record(&state, id).await?;
    audit::record(
        &state,
        &caller.actor(),
        "message.erased",
        Some(id),
        Some(record.channel),
        Some(format!("reason: {reason}")),
    )
    .await?;
    state
        .inner
        .engine
        .store()
        .run(move |store| store.erase(&[id]))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
