//! Writing the audit trail.

use oxim_model::{ChannelId, MessageId};
use oxim_store::AuditEvent;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Appends an audit event. Failures are returned, so actions that must be
/// audited (such as viewing patient data) fail closed.
pub(crate) async fn record(
    state: &AppState,
    actor: &str,
    action: &str,
    message_id: Option<MessageId>,
    channel: Option<ChannelId>,
    detail: Option<String>,
) -> ApiResult<()> {
    let event = AuditEvent {
        at: state.inner.engine.clock().now(),
        action: action.to_owned(),
        actor: actor.to_owned(),
        message_id,
        channel,
        detail,
    };
    state
        .inner
        .engine
        .store()
        .run(move |store| store.record_audit(&event))
        .await
        .map_err(ApiError::from)
}

/// Appends an audit event, logging instead of failing.
pub(crate) async fn record_best_effort(
    state: &AppState,
    actor: &str,
    action: &str,
    channel: Option<ChannelId>,
    detail: Option<String>,
) {
    if let Err(error) = record(state, actor, action, None, channel, detail).await {
        tracing::warn!(%action, error = %error.message, "cannot write audit event");
    }
}
