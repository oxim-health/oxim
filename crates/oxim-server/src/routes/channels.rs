//! Channel configuration and deployment.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use oxim_auth::Permission;
use oxim_core::{ChannelConfig, EngineError};
use oxim_model::{ChannelId, ConnectorId};
use serde_json::{Value, json};

use crate::audit;
use crate::caller::Caller;
use crate::error::{ApiError, ApiResult};
use crate::extract::{PathParam, TextBody};
use crate::files::{atomic_write, channel_files, find_channel};
use crate::state::AppState;

fn channel_id(id: &str) -> ApiResult<ChannelId> {
    ChannelId::new(id).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Queue statistics of one destination as JSON.
pub(crate) async fn queue_json(
    state: &AppState,
    channel: &ChannelId,
    destination: &ConnectorId,
) -> Value {
    let (c, d) = (channel.clone(), destination.clone());
    match state
        .inner
        .engine
        .store()
        .run(move |store| store.queue_stats(&c, &d))
        .await
    {
        Ok(stats) => json!({
            "queued": stats.queued,
            "sending": stats.sending,
            "retrying": stats.retrying,
            "failed": stats.failed,
            "oldest_pending_at": stats.oldest_pending_at,
        }),
        Err(_) => Value::Null,
    }
}

/// A summary of every channel file with its deployment state and queues.
pub(crate) async fn channel_summaries(state: &AppState) -> Vec<Value> {
    let deployed: BTreeSet<ChannelId> = state.inner.engine.deployed().await.into_iter().collect();
    let mut summaries = Vec::new();
    for file in channel_files(&state.inner.config.channels_dir) {
        let name = file_name(&file.path);
        match file.parsed {
            Ok(config) => {
                let mut destinations = Vec::new();
                for destination in &config.destinations {
                    destinations.push(json!({
                        "id": destination.id,
                        "type": destination.kind,
                        "queue": queue_json(state, &config.id, &destination.id).await,
                    }));
                }
                summaries.push(json!({
                    "id": config.id,
                    "name": config.name,
                    "description": config.description,
                    "file": name,
                    "enabled": config.enabled,
                    "deployed": deployed.contains(&config.id),
                    "source": {
                        "id": config.source.id,
                        "type": config.source.kind,
                        "data_type": config.source.data_type,
                    },
                    "destinations": destinations,
                }));
            }
            Err(error) => summaries.push(json!({ "file": name, "error": error })),
        }
    }
    summaries
}

/// `GET /channels`
pub(crate) async fn list(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewChannels)?;
    Ok(Json(json!({ "channels": channel_summaries(&state).await })))
}

/// `GET /channels/{id}`: the YAML may hold credentials (HTTP headers, for
/// example), so only callers who may edit channels can read it.
pub(crate) async fn get(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::EditChannels)?;
    let id = channel_id(&id)?;
    let file = find_channel(&state.inner.config.channels_dir, id.as_str())
        .ok_or_else(|| ApiError::not_found(format!("channel {id} not found")))?;
    let deployed = state.inner.engine.deployed().await.contains(&id);
    Ok(Json(json!({
        "id": id,
        "file": file_name(&file.path),
        "yaml": file.text,
        "deployed": deployed,
    })))
}

/// Validates a channel as the engine would deploy it, without side
/// effects.
fn validate(state: &AppState, config: &ChannelConfig) -> Result<(), EngineError> {
    let registry = state.inner.engine.registry();
    registry.compile(config)?;
    registry.source(&config.source)?;
    for destination in &config.destinations {
        registry.destination(destination)?;
    }
    Ok(())
}

/// `PUT /channels/{id}`: the body is the channel YAML. The file watcher of
/// the `oxim` program redeploys the channel.
pub(crate) async fn put(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
    TextBody(body): TextBody,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::EditChannels)?;
    let id = channel_id(&id)?;
    let config = ChannelConfig::from_yaml(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_channel", e.to_string()))?;
    if config.id != id {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_channel",
            format!("the YAML defines channel {}, not {id}", config.id),
        ));
    }
    validate(&state, &config)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_channel", e.to_string()))?;
    let dir = state.inner.config.channels_dir.clone();
    let path = find_channel(&dir, id.as_str())
        .map(|file| file.path)
        .unwrap_or_else(|| dir.join(format!("{id}.yaml")));
    atomic_write(&path, body.as_bytes()).map_err(ApiError::internal)?;
    audit::record(
        &state,
        &caller.actor(),
        "channel.saved",
        None,
        Some(id.clone()),
        Some(file_name(&path)),
    )
    .await?;
    Ok(Json(json!({ "id": id, "file": file_name(&path) })))
}

async fn load(state: &AppState, id: &ChannelId) -> ApiResult<ChannelConfig> {
    let file = find_channel(&state.inner.config.channels_dir, id.as_str())
        .ok_or_else(|| ApiError::not_found(format!("channel {id} not found")))?;
    file.parsed
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_channel", e))
}

/// `POST /channels/{id}/deploy`
pub(crate) async fn deploy(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::DeployChannels)?;
    let id = channel_id(&id)?;
    let config = load(&state, &id).await?;
    state.inner.engine.deploy(config).await?;
    audit::record(
        &state,
        &caller.actor(),
        "channel.deployed",
        None,
        Some(id),
        None,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /channels/{id}/redeploy`
pub(crate) async fn redeploy(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::DeployChannels)?;
    let id = channel_id(&id)?;
    let config = load(&state, &id).await?;
    state.inner.engine.redeploy(config).await?;
    audit::record(
        &state,
        &caller.actor(),
        "channel.redeployed",
        None,
        Some(id),
        None,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /channels/{id}/undeploy`
pub(crate) async fn undeploy(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::DeployChannels)?;
    let id = channel_id(&id)?;
    state.inner.engine.undeploy(&id).await?;
    audit::record(
        &state,
        &caller.actor(),
        "channel.undeployed",
        None,
        Some(id),
        None,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /channels/{id}`: undeploys the channel and moves its file to
/// `channels_dir/.deleted/`.
pub(crate) async fn delete(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(id): PathParam<String>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::EditChannels)?;
    caller.require(Permission::DeployChannels)?;
    let id = channel_id(&id)?;
    let dir = state.inner.config.channels_dir.clone();
    let file = find_channel(&dir, id.as_str())
        .ok_or_else(|| ApiError::not_found(format!("channel {id} not found")))?;
    match state.inner.engine.undeploy(&id).await {
        Ok(()) | Err(EngineError::NotDeployed(_)) => {}
        Err(other) => return Err(other.into()),
    }
    let archive = dir.join(".deleted");
    std::fs::create_dir_all(&archive).map_err(ApiError::internal)?;
    let stamp = state.inner.engine.clock().now().unix_millis();
    let target = archive.join(format!("{}.{stamp}", file_name(&file.path)));
    std::fs::rename(&file.path, &target).map_err(ApiError::internal)?;
    audit::record(
        &state,
        &caller.actor(),
        "channel.deleted",
        None,
        Some(id),
        Some(file_name(&target)),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
