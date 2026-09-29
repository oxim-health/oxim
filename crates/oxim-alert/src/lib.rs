//! The OXIM alert engine.
//!
//! Rules watch the running engine and notify targets when something needs
//! an operator:
//!
//! | Rule `kind` | Fires when |
//! |---|---|
//! | `queue_depth` | more than `above` messages wait for a destination (queued, sending or retrying) |
//! | `queue_age` | the oldest waiting message of a destination is older than `older_than` |
//! | `failed_deliveries` | more than `above` deliveries were given up and wait for an operator |
//! | `error_rate` | more than `above` messages received within `within` ended in error |
//! | `device_silence` | a device tracked with `track-device` and `silence_after` has been silent too long |
//! | `disk_space` | less than `below` (bytes or a percentage) is free on the data volume or `path` |
//! | `certificate_expiry` | a certificate (the web server's and `files`) expires within `within` |
//!
//! Queue rules take optional `channel` and `destination` filters. A rule
//! fires once its condition has held for `for` (default: at once), is
//! notified again every `repeat` while it holds, and is notified as
//! resolved when it ends.
//!
//! | Target `type` | Sends |
//! |---|---|
//! | `log` | a log entry |
//! | `webhook` | a JSON POST (`url`, `headers`, `ca_file`) |
//! | `teams` | an Adaptive Card to a Microsoft Teams webhook or workflow URL |
//! | `slack` | a message to a Slack incoming webhook |
//! | `email` | an email through the `smtp` destination connector (`settings`) |
//! | `destination` | text or JSON through any destination connector (`connector`, `settings`, `format`) |
//! | `syslog` | an RFC 5424 message over UDP or TCP (`address`, `protocol`, `facility`) |
//! | `snmp` | an SNMPv2c trap (`address`, `community_env`, `trap_oid`) |
//!
//! ```yaml
//! alerts:
//!   interval: 1m
//!   repeat: 1h
//!   targets:
//!     - {id: ops, type: teams, url: "https://example.webhook.office.com/..."}
//!     - {id: noc, type: syslog, address: "10.0.0.5:514", min_severity: critical}
//!   rules:
//!     - {id: lis-backlog, kind: queue_depth, destination: lis, above: 100, for: 5m}
//!     - {id: analyzers, kind: device_silence, severity: critical}
//!     - {id: disk, kind: disk_space, below: "10%"}
//!     - {id: certificates, kind: certificate_expiry, within: 30d}
//! ```
//!
//! Notifications carry states and counts, never message contents, so no
//! patient data leaves OXIM through alerts.

mod config;
mod evaluate;
mod notify;
mod snapshot;
mod snmp;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{Engine, EngineError, Registry};
use oxim_devices::DeviceEnvironment;
use oxim_model::{ChannelId, MessageStatus, Timestamp};
use oxim_store::MessageQuery;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub use config::{
    AlertSettings, Condition, DEFAULT_TRAP_OID, Format, Protocol, RuleConfig, Severity, Space,
    TargetConfig, TargetKind,
};
pub use evaluate::{ActiveAlert, AlertState, Evaluator, Notification, human};
pub use snapshot::{
    CertificateSample, DiskSample, ErrorSample, QueueSample, Snapshot, certificate, disk, not_after,
};

/// What the alert engine reads from.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The running engine: queues and message errors.
    pub engine: Engine,
    /// The device registry, for silence rules.
    pub devices: Option<DeviceEnvironment>,
    /// The data directory, the default volume of disk rules.
    pub data_dir: PathBuf,
    /// Certificates always checked by expiry rules, such as the web
    /// server's.
    pub certificates: Vec<PathBuf>,
}

/// Evaluates the rules and sends notifications.
#[derive(Debug)]
pub struct AlertEngine {
    settings: AlertSettings,
    evaluator: Mutex<Evaluator>,
    targets: Vec<Arc<notify::Target>>,
    sequence: AtomicU64,
}

impl AlertEngine {
    /// Validates the settings and builds the targets; destination-based
    /// targets use connector types from `registry`.
    pub fn new(settings: AlertSettings, registry: &Registry) -> Result<Self, EngineError> {
        settings.validate()?;
        let targets = settings
            .targets
            .iter()
            .map(|target| notify::Target::new(target, registry).map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            settings,
            evaluator: Mutex::new(Evaluator::default()),
            targets,
            sequence: AtomicU64::new(1),
        })
    }

    /// The settings in use.
    pub fn settings(&self) -> &AlertSettings {
        &self.settings
    }

    /// The alerts whose conditions currently hold.
    pub fn active(&self) -> Vec<ActiveAlert> {
        self.evaluator
            .lock()
            .map(|evaluator| evaluator.active(&self.settings))
            .unwrap_or_default()
    }

    /// Evaluates `snapshot` and sends the resulting notifications. Returns
    /// them.
    pub fn process(&self, snapshot: &Snapshot) -> Vec<Notification> {
        let notifications = match self.evaluator.lock() {
            Ok(mut evaluator) => evaluator.evaluate(&self.settings, snapshot),
            Err(_) => return Vec::new(),
        };
        for notification in &notifications {
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
            notify::dispatch(&self.targets, notification, sequence);
        }
        notifications
    }

    /// Reads everything the rules need.
    pub async fn collect(&self, sources: &Sources) -> Snapshot {
        collect(&self.settings, sources).await
    }

    /// Evaluates the rules every `interval` until `cancel` fires.
    pub async fn run(self: Arc<Self>, sources: Sources, cancel: CancellationToken) {
        if self.settings.rules.is_empty() {
            return;
        }
        let mut ticks = tokio::time::interval(self.settings.interval.0);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = ticks.tick() => {}
            }
            let snapshot = self.collect(&sources).await;
            let sent = self.process(&snapshot);
            if !sent.is_empty() {
                debug!(notifications = sent.len(), "alerts evaluated");
            }
        }
    }
}

fn needs_queues(settings: &AlertSettings) -> bool {
    settings.rules.iter().any(|rule| {
        matches!(
            rule.condition,
            Condition::QueueDepth { .. }
                | Condition::QueueAge { .. }
                | Condition::FailedDeliveries { .. }
        )
    })
}

async fn collect(settings: &AlertSettings, sources: &Sources) -> Snapshot {
    let engine = &sources.engine;
    let now = engine.clock().now();
    let mut snapshot = Snapshot {
        now: Some(now),
        ..Snapshot::default()
    };
    let channels = engine.deployed().await;
    if needs_queues(settings) {
        for channel in &channels {
            let Some(config) = engine.channel_config(channel).await else {
                continue;
            };
            for destination in &config.destinations {
                let (c, d) = (channel.clone(), destination.id.clone());
                match engine
                    .store()
                    .run(move |store| store.queue_stats(&c, &d))
                    .await
                {
                    Ok(stats) => snapshot.queues.push(QueueSample {
                        channel: channel.clone(),
                        destination: destination.id.clone(),
                        stats,
                    }),
                    Err(e) => {
                        warn!(%channel, destination = %destination.id, error = %e, "alerts: cannot read queue")
                    }
                }
            }
        }
    }
    for rule in &settings.rules {
        match &rule.condition {
            Condition::ErrorRate {
                channel,
                above,
                within,
            } => {
                let from = Timestamp::from_unix_nanos(
                    now.unix_nanos()
                        .saturating_sub(i64::try_from(within.0.as_nanos()).unwrap_or(i64::MAX)),
                );
                let watched: Vec<ChannelId> = match channel {
                    Some(name) => ChannelId::new(name.clone()).into_iter().collect(),
                    None => channels.clone(),
                };
                for channel in watched {
                    let query = MessageQuery {
                        channel: Some(channel.clone()),
                        status: Some(MessageStatus::Error),
                        from: Some(from),
                        limit: usize::try_from(above.saturating_add(1))
                            .unwrap_or(usize::MAX)
                            .min(10_000),
                        ..MessageQuery::default()
                    };
                    match engine
                        .store()
                        .run(move |store| store.list_messages(&query))
                        .await
                    {
                        Ok(messages) => snapshot.errors.push(ErrorSample {
                            rule: rule.id.clone(),
                            channel,
                            errors: messages.len() as u64,
                        }),
                        Err(e) => warn!(%channel, error = %e, "alerts: cannot count errors"),
                    }
                }
            }
            Condition::DiskSpace { path, .. } => {
                let path = path.clone().unwrap_or_else(|| sources.data_dir.clone());
                let rule_id = rule.id.clone();
                let reading = tokio::task::spawn_blocking(move || {
                    disk(&rule_id, &path).map_err(|e| (path, e))
                })
                .await;
                match reading {
                    Ok(Ok(sample)) => snapshot.disks.push(sample),
                    Ok(Err((path, e))) => {
                        warn!(path = %path.display(), error = %e, "alerts: cannot read free space");
                    }
                    Err(e) => warn!(error = %e, "alerts: cannot read free space"),
                }
            }
            Condition::CertificateExpiry { files, .. } => {
                let mut paths: Vec<PathBuf> = sources.certificates.clone();
                paths.extend(files.iter().cloned());
                paths.sort();
                paths.dedup();
                let rule_id = rule.id.clone();
                if let Ok(samples) = tokio::task::spawn_blocking(move || {
                    paths
                        .iter()
                        .map(|path| certificate(&rule_id, path))
                        .collect::<Vec<_>>()
                })
                .await
                {
                    snapshot.certificates.extend(samples);
                }
            }
            Condition::DeviceSilence { .. } => {}
            Condition::QueueDepth { .. }
            | Condition::QueueAge { .. }
            | Condition::FailedDeliveries { .. } => {}
        }
    }
    if settings
        .rules
        .iter()
        .any(|rule| matches!(rule.condition, Condition::DeviceSilence { .. }))
        && let Some(devices) = &sources.devices
    {
        match devices.silent(now) {
            Ok(silent) => snapshot.silent_devices = silent,
            Err(e) => warn!(error = %e, "alerts: cannot read the device registry"),
        }
    }
    snapshot
}

/// How long to wait between evaluations at most, for callers that poll.
pub const MAX_INTERVAL: Duration = Duration::from_secs(24 * 3600);
