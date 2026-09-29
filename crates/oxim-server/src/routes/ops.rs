//! Operations: alerts, the device registry, backups and maintenance mode.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use oxim_auth::Permission;
use oxim_model::{ChannelId, DeviceId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit;
use crate::backup::{self, BackupSources};
use crate::caller::Caller;
use crate::error::{ApiError, ApiResult};
use crate::extract::{JsonBody, PathParam};
use crate::state::{AppState, Maintenance};

/// `GET /alerts`: the configured rules and targets (without their
/// settings, which may hold webhook secrets) and the active alerts.
pub(crate) async fn alerts(
    State(state): State<AppState>,
    caller: Caller,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewSystem)?;
    let Some(engine) = &state.inner.services.alerts else {
        return Ok(Json(json!({
            "enabled": false,
            "interval_seconds": null,
            "repeat_seconds": null,
            "rules": [],
            "targets": [],
            "active": [],
        })));
    };
    let settings = engine.settings();
    let rules: Vec<Value> = settings
        .rules
        .iter()
        .map(|rule| {
            json!({
                "id": rule.id,
                "kind": rule.condition.kind(),
                "severity": rule.severity,
                "hold_seconds": rule.hold.map(|d| d.0.as_secs()),
                "targets": rule.targets,
                "condition": serde_json::to_value(&rule.condition).unwrap_or(Value::Null),
            })
        })
        .collect();
    let targets: Vec<Value> = settings
        .targets
        .iter()
        .map(|target| {
            let kind = serde_json::to_value(&target.kind)
                .ok()
                .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            json!({ "id": target.id, "type": kind, "min_severity": target.min_severity })
        })
        .collect();
    Ok(Json(json!({
        "enabled": !settings.rules.is_empty(),
        "interval_seconds": settings.interval.0.as_secs(),
        "repeat_seconds": settings.repeat.0.as_secs(),
        "rules": rules,
        "targets": targets,
        "active": engine.active(),
    })))
}

/// `GET /devices`: every known and declared device with its status.
pub(crate) async fn devices(
    State(state): State<AppState>,
    caller: Caller,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewDashboard)?;
    let Some(devices) = state.inner.services.devices.clone() else {
        return Ok(Json(json!({ "devices": [] })));
    };
    let now = state.inner.engine.clock().now();
    let snapshot = tokio::task::spawn_blocking(move || devices.snapshot(now))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "devices": snapshot })))
}

/// `DELETE /devices/{channel}/{device}`: removes a device from the
/// registry, for example after it was replaced. Audited.
pub(crate) async fn forget_device(
    State(state): State<AppState>,
    caller: Caller,
    PathParam((channel, device)): PathParam<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::DeployChannels)?;
    let channel = ChannelId::new(channel).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let device = DeviceId::new(device).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let Some(devices) = state.inner.services.devices.clone() else {
        return Err(ApiError::not_found("the device registry is not available"));
    };
    let (c, d) = (channel.clone(), device.clone());
    let forgotten = tokio::task::spawn_blocking(move || {
        devices
            .registry()
            .and_then(|registry| registry.forget(&c, &d))
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    if !forgotten {
        return Err(ApiError::not_found(format!(
            "device {device} is not known on channel {channel}"
        )));
    }
    audit::record(
        &state,
        &caller.actor(),
        "device.forgotten",
        None,
        Some(channel),
        Some(device.to_string()),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn sources(state: &AppState) -> BackupSources {
    let config = &state.inner.config;
    BackupSources {
        data_dir: config.data_dir.clone(),
        channels_dir: config.channels_dir.clone(),
        tables_dir: config.tables_dir.clone(),
        scripts_dir: config.scripts_dir.clone(),
        config_file: config.config_file.clone(),
    }
}

/// `GET /backups`
pub(crate) async fn backups(
    State(state): State<AppState>,
    caller: Caller,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ManageSystem)?;
    let settings = state.inner.config.backups.clone();
    let list = tokio::task::spawn_blocking(move || backup::list(&settings.dir))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "backups": list,
        "keep": state.inner.config.backups.keep,
    })))
}

/// `POST /backups`: takes a backup now and deletes the oldest beyond
/// `keep`. Audited.
pub(crate) async fn create_backup(
    State(state): State<AppState>,
    caller: Caller,
) -> ApiResult<impl IntoResponse> {
    caller.require(Permission::ManageSystem)?;
    let now = state.inner.engine.clock().now();
    let settings = state.inner.config.backups.clone();
    let name = backup::backup_name(now);
    let out = settings.dir.join(&name);
    let sources = sources(&state);
    let manifest = tokio::task::spawn_blocking(move || {
        let manifest = backup::create(&sources, &out, now)?;
        backup::prune(&settings.dir, settings.keep.max(1))?;
        let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        Ok::<_, backup::BackupError>((manifest, size))
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    audit::record(
        &state,
        &caller.actor(),
        "backup.created",
        None,
        None,
        Some(name.clone()),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "name": name,
            "size": manifest.1,
            "files": manifest.0.files.len(),
            "created_at": manifest.0.created_at,
        })),
    ))
}

/// `GET /backups/{name}`: downloads a backup. Audited, since a backup
/// holds patient data.
pub(crate) async fn download_backup(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(name): PathParam<String>,
) -> ApiResult<Response> {
    caller.require(Permission::ManageSystem)?;
    if !backup::valid_name(&name) {
        return Err(ApiError::bad_request("not a backup file name"));
    }
    let path = state.inner.config.backups.dir.join(&name);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found(format!("backup {name} not found")))?;
    let size = file.metadata().await.map(|m| m.len()).ok();
    audit::record(
        &state,
        &caller.actor(),
        "backup.downloaded",
        None,
        None,
        Some(name.clone()),
    )
    .await?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Some(size) = size
        && let Ok(value) = HeaderValue::from_str(&size.to_string())
    {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    Ok(response)
}

/// The maintenance state as JSON.
pub(crate) fn maintenance_json(state: &AppState) -> Value {
    let paused = state.inner.engine.sources_paused();
    let info = state
        .inner
        .maintenance
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    match (paused, info) {
        (true, Some(info)) => json!({
            "enabled": true,
            "reason": info.reason,
            "by": info.by,
            "since": info.since,
        }),
        (true, None) => json!({ "enabled": true, "reason": null, "by": null, "since": null }),
        (false, _) => json!({ "enabled": false, "reason": null, "by": null, "since": null }),
    }
}

/// Body of `POST /system/maintenance`.
#[derive(Deserialize)]
pub(crate) struct MaintenanceRequest {
    enabled: bool,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /system/maintenance`: stops or restarts every source. Delivery
/// of stored messages goes on. Not persisted across restarts. Audited.
pub(crate) async fn set_maintenance(
    State(state): State<AppState>,
    caller: Caller,
    JsonBody(request): JsonBody<MaintenanceRequest>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ManageSystem)?;
    let engine = &state.inner.engine;
    if request.enabled {
        let reason = request
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|r| r.len() >= 3)
            .ok_or_else(|| ApiError::bad_request("a reason of at least 3 characters is required"))?
            .to_owned();
        engine.pause_sources();
        if let Ok(mut guard) = state.inner.maintenance.lock() {
            *guard = Some(Maintenance {
                reason: reason.clone(),
                by: caller.actor(),
                since: engine.clock().now(),
            });
        }
        audit::record(
            &state,
            &caller.actor(),
            "maintenance.started",
            None,
            None,
            Some(reason),
        )
        .await?;
    } else {
        engine.resume_sources();
        if let Ok(mut guard) = state.inner.maintenance.lock() {
            *guard = None;
        }
        audit::record(
            &state,
            &caller.actor(),
            "maintenance.ended",
            None,
            None,
            None,
        )
        .await?;
    }
    Ok(Json(maintenance_json(&state)))
}
