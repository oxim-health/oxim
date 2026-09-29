//! Code tables: CSV files in the tables directory.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use oxim_auth::Permission;
use oxim_transform::CodeTable;
use serde_json::{Value, json};

use crate::audit;
use crate::caller::Caller;
use crate::error::{ApiError, ApiResult};
use crate::extract::{PathParam, TextBody};
use crate::files::{atomic_write, safe_file_name};
use crate::state::AppState;

fn table_path(state: &AppState, name: &str) -> ApiResult<std::path::PathBuf> {
    if !safe_file_name(name, &["csv"]) {
        return Err(ApiError::bad_request(
            "table names are plain file names ending in .csv",
        ));
    }
    Ok(state.inner.config.tables_dir.join(name))
}

/// `GET /tables`
pub(crate) async fn list(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewTables)?;
    let mut tables = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&state.inner.config.tables_dir) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if !path.is_file() || !safe_file_name(&name, &["csv"]) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let (entries, error) = match CodeTable::from_csv(&text) {
                Ok(table) => (Some(table.len()), None),
                Err(error) => (None, Some(error.to_string())),
            };
            tables.push(json!({
                "name": name,
                "size": text.len(),
                "entries": entries,
                "error": error,
            }));
        }
    }
    tables.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(Json(json!({ "tables": tables })))
}

/// `GET /tables/{name}`
pub(crate) async fn get(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(name): PathParam<String>,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::ViewTables)?;
    let path = table_path(&state, &name)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| ApiError::not_found(format!("table {name} not found")))?;
    Ok(Json(json!({ "name": name, "csv": text })))
}

/// `PUT /tables/{name}`: the body is the CSV text, validated before it
/// replaces the file. Channels use the new version when redeployed.
pub(crate) async fn put(
    State(state): State<AppState>,
    caller: Caller,
    PathParam(name): PathParam<String>,
    TextBody(body): TextBody,
) -> ApiResult<Json<Value>> {
    caller.require(Permission::EditTables)?;
    let path = table_path(&state, &name)?;
    let table = CodeTable::from_csv(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_table", e.to_string()))?;
    atomic_write(&path, body.as_bytes()).map_err(ApiError::internal)?;
    audit::record(
        &state,
        &caller.actor(),
        "table.saved",
        None,
        None,
        Some(name.clone()),
    )
    .await?;
    Ok(Json(json!({ "name": name, "entries": table.len() })))
}
