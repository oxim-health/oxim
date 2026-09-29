//! Prometheus metrics, computed from the message store on each scrape plus
//! a few in-memory counters.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use oxim_auth::Permission;
use oxim_model::{ChannelId, ConnectorId};

use crate::caller::Caller;
use crate::error::ApiResult;
use crate::files::channel_files;
use crate::state::AppState;

/// Counters kept in memory since the server started.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    /// HTTP responses by status class (1xx to 5xx).
    responses: [AtomicU64; 5],
    /// Failed logins.
    pub(crate) login_failures: AtomicU64,
}

impl Counters {
    pub(crate) fn record_status(&self, status: u16) {
        let class = usize::from(status / 100).clamp(1, 5) - 1;
        self.responses[class].fetch_add(1, Ordering::Relaxed);
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn header_lines(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// The metrics text in the Prometheus exposition format.
pub(crate) async fn render(state: &AppState) -> ApiResult<String> {
    let inner = &state.inner;
    let counts = inner
        .engine
        .store()
        .run(|store| store.status_counts())
        .await?;
    let deployed: BTreeSet<ChannelId> = inner.engine.deployed().await.into_iter().collect();
    let mut destinations: Vec<(ChannelId, ConnectorId)> = Vec::new();
    let mut known: BTreeSet<ChannelId> = deployed.clone();
    for file in channel_files(&inner.config.channels_dir) {
        if let Ok(config) = file.parsed {
            for destination in &config.destinations {
                destinations.push((config.id.clone(), destination.id.clone()));
            }
            known.insert(config.id);
        }
    }
    let now = inner.engine.clock().now();
    let mut oldest = Vec::new();
    for (channel, destination) in &destinations {
        let (c, d) = (channel.clone(), destination.clone());
        let stats = inner
            .engine
            .store()
            .run(move |store| store.queue_stats(&c, &d))
            .await?;
        let age = stats.oldest_pending_at.map_or(0.0, |at| {
            (now.unix_nanos().saturating_sub(at.unix_nanos())).max(0) as f64 / 1e9
        });
        oldest.push((channel, destination, age));
    }

    let mut out = String::new();
    header_lines(
        &mut out,
        "oxim_build_info",
        "gauge",
        "Version of the running OXIM.",
    );
    let _ = writeln!(
        out,
        "oxim_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );
    header_lines(
        &mut out,
        "oxim_uptime_seconds",
        "gauge",
        "Seconds since the server started.",
    );
    let _ = writeln!(
        out,
        "oxim_uptime_seconds {}",
        inner.started.elapsed().as_secs()
    );
    header_lines(
        &mut out,
        "oxim_channel_deployed",
        "gauge",
        "Whether a channel is deployed (1) or not (0).",
    );
    for channel in &known {
        let _ = writeln!(
            out,
            "oxim_channel_deployed{{channel=\"{}\"}} {}",
            escape(channel.as_str()),
            u8::from(deployed.contains(channel))
        );
    }
    header_lines(
        &mut out,
        "oxim_messages",
        "gauge",
        "Stored messages by channel and status.",
    );
    for (channel, status, count) in &counts.messages {
        let _ = writeln!(
            out,
            "oxim_messages{{channel=\"{}\",status=\"{}\"}} {count}",
            escape(channel.as_str()),
            status.as_str()
        );
    }
    header_lines(
        &mut out,
        "oxim_deliveries",
        "gauge",
        "Stored deliveries by channel, destination and status.",
    );
    for (channel, destination, status, count) in &counts.deliveries {
        let _ = writeln!(
            out,
            "oxim_deliveries{{channel=\"{}\",destination=\"{}\",status=\"{}\"}} {count}",
            escape(channel.as_str()),
            escape(destination.as_str()),
            status.as_str()
        );
    }
    header_lines(
        &mut out,
        "oxim_queue_oldest_pending_seconds",
        "gauge",
        "Age of the oldest message not yet sent to a destination (0 when the queue is empty).",
    );
    for (channel, destination, age) in &oldest {
        let _ = writeln!(
            out,
            "oxim_queue_oldest_pending_seconds{{channel=\"{}\",destination=\"{}\"}} {age:.3}",
            escape(channel.as_str()),
            escape(destination.as_str())
        );
    }
    header_lines(
        &mut out,
        "oxim_http_responses_total",
        "counter",
        "HTTP responses by status class.",
    );
    for (index, counter) in inner.counters.responses.iter().enumerate() {
        let _ = writeln!(
            out,
            "oxim_http_responses_total{{class=\"{}xx\"}} {}",
            index + 1,
            counter.load(Ordering::Relaxed)
        );
    }
    header_lines(
        &mut out,
        "oxim_login_failures_total",
        "counter",
        "Failed login attempts.",
    );
    let _ = writeln!(
        out,
        "oxim_login_failures_total {}",
        inner.counters.login_failures.load(Ordering::Relaxed)
    );
    Ok(out)
}

/// `GET /metrics`: needs a principal with system access; Prometheus can
/// send an API token as a bearer token.
pub(crate) async fn metrics(State(state): State<AppState>, caller: Caller) -> ApiResult<Response> {
    caller.require(Permission::ViewSystem)?;
    let text = render(&state).await?;
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
        .into_response())
}
