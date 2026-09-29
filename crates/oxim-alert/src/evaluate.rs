//! Rule evaluation: which subjects breach a rule, and when an alert fires,
//! repeats and resolves.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use oxim_model::Timestamp;
use serde::Serialize;

use crate::config::{AlertSettings, Condition, RuleConfig, Severity, Space};
use crate::snapshot::Snapshot;

/// Whether an alert started or ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// The condition holds.
    Firing,
    /// The condition no longer holds.
    Resolved,
}

impl AlertState {
    /// The display name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Firing => "FIRING",
            Self::Resolved => "RESOLVED",
        }
    }
}

/// A notification to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notification {
    /// The rule.
    pub rule: String,
    /// The rule kind, for example `queue_depth`.
    pub kind: &'static str,
    /// What breached the rule, for example `lab-results/lis` or `chem-1`.
    pub subject: String,
    /// The rule's severity.
    pub severity: Severity,
    /// Firing or resolved.
    pub state: AlertState,
    /// One line describing the situation.
    pub summary: String,
    /// When the condition was first seen.
    pub since: Timestamp,
    /// When the notification was produced.
    pub at: Timestamp,
    /// Whether this repeats an earlier notification of the same alert.
    pub repeat: bool,
    /// Target identifiers; all targets when empty.
    #[serde(skip)]
    pub targets: Vec<String>,
}

impl Notification {
    /// One line of text: state, severity, rule, subject and summary.
    pub fn text(&self) -> String {
        format!(
            "{} {} {} [{}]: {}",
            self.state.as_str(),
            self.severity.as_str(),
            self.rule,
            self.subject,
            self.summary
        )
    }
}

/// An alert whose condition currently holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveAlert {
    /// The rule.
    pub rule: String,
    /// The rule kind.
    pub kind: &'static str,
    /// What breaches the rule.
    pub subject: String,
    /// The rule's severity.
    pub severity: Severity,
    /// The latest description.
    pub summary: String,
    /// When the condition was first seen.
    pub since: Timestamp,
    /// Whether it fired (`false` while its `for` period runs).
    pub firing: bool,
    /// When it was last notified.
    pub notified_at: Option<Timestamp>,
}

#[derive(Debug, Clone)]
struct State {
    since: Timestamp,
    firing: bool,
    notified_at: Option<Timestamp>,
    summary: String,
}

/// Keeps the state of every alert between evaluations.
#[derive(Debug, Default)]
pub struct Evaluator {
    states: BTreeMap<(String, String), State>,
}

fn elapsed(from: Timestamp, to: Timestamp) -> Duration {
    u64::try_from(to.unix_nanos().saturating_sub(from.unix_nanos()))
        .map(Duration::from_nanos)
        .unwrap_or_default()
}

/// A duration for people: `45s`, `12m`, `3h 5m`, `2d 4h`.
pub fn human(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => {
            let minutes = (seconds % 3600) / 60;
            if minutes == 0 {
                format!("{}h", seconds / 3600)
            } else {
                format!("{}h {minutes}m", seconds / 3600)
            }
        }
        _ => {
            let hours = (seconds % 86_400) / 3600;
            if hours == 0 {
                format!("{}d", seconds / 86_400)
            } else {
                format!("{}d {hours}h", seconds / 86_400)
            }
        }
    }
}

fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn matches(filter: Option<&String>, value: &str) -> bool {
    filter.is_none_or(|wanted| wanted == value)
}

/// The subjects that breach `rule`, with a description of each.
fn breaches(rule: &RuleConfig, snapshot: &Snapshot, now: Timestamp) -> Vec<(String, String)> {
    let mut found = Vec::new();
    match &rule.condition {
        Condition::QueueDepth {
            channel,
            destination,
            above,
        } => {
            for queue in &snapshot.queues {
                if !matches(channel.as_ref(), queue.channel.as_str())
                    || !matches(destination.as_ref(), queue.destination.as_str())
                {
                    continue;
                }
                let depth = queue.stats.queued + queue.stats.sending + queue.stats.retrying;
                if depth > *above {
                    found.push((
                        format!("{}/{}", queue.channel, queue.destination),
                        format!(
                            "{depth} messages are waiting for destination {} of channel {} (limit {above})",
                            queue.destination, queue.channel
                        ),
                    ));
                }
            }
        }
        Condition::QueueAge {
            channel,
            destination,
            older_than,
        } => {
            for queue in &snapshot.queues {
                if !matches(channel.as_ref(), queue.channel.as_str())
                    || !matches(destination.as_ref(), queue.destination.as_str())
                {
                    continue;
                }
                if let Some(oldest) = queue.stats.oldest_pending_at {
                    let age = elapsed(oldest, now);
                    if age > older_than.0 {
                        found.push((
                            format!("{}/{}", queue.channel, queue.destination),
                            format!(
                                "the oldest message waiting for destination {} of channel {} arrived {} ago (limit {})",
                                queue.destination,
                                queue.channel,
                                human(age),
                                human(older_than.0)
                            ),
                        ));
                    }
                }
            }
        }
        Condition::FailedDeliveries {
            channel,
            destination,
            above,
        } => {
            for queue in &snapshot.queues {
                if !matches(channel.as_ref(), queue.channel.as_str())
                    || !matches(destination.as_ref(), queue.destination.as_str())
                {
                    continue;
                }
                if queue.stats.failed > *above {
                    found.push((
                        format!("{}/{}", queue.channel, queue.destination),
                        format!(
                            "{} deliveries to destination {} of channel {} failed and wait for an operator",
                            queue.stats.failed, queue.destination, queue.channel
                        ),
                    ));
                }
            }
        }
        Condition::ErrorRate { above, within, .. } => {
            for sample in snapshot.errors.iter().filter(|s| s.rule == rule.id) {
                if sample.errors > *above {
                    found.push((
                        sample.channel.to_string(),
                        format!(
                            "more than {above} messages of channel {} ended in error within {}",
                            sample.channel,
                            human(within.0)
                        ),
                    ));
                }
            }
        }
        Condition::DeviceSilence { device } => {
            for state in &snapshot.silent_devices {
                if !matches(device.as_ref(), state.device.as_str()) {
                    continue;
                }
                let subject = match &state.channel {
                    Some(channel) => format!("{}@{channel}", state.device),
                    None => state.device.to_string(),
                };
                let window = state.silence_after.map(human).unwrap_or_default();
                let summary = match state.last_seen {
                    Some(last) => format!(
                        "device {} has sent nothing for {} (silence window {window})",
                        state.device,
                        human(elapsed(last, now))
                    ),
                    None => format!("device {} has never sent a message", state.device),
                };
                found.push((subject, summary));
            }
        }
        Condition::DiskSpace { below, .. } => {
            for disk in snapshot.disks.iter().filter(|d| d.rule == rule.id) {
                let low = match below {
                    Space::Bytes(limit) => disk.available < *limit,
                    Space::Percent(percent) => {
                        disk.total > 0
                            && (disk.available as f64) * 100.0 < percent * disk.total as f64
                    }
                };
                if low {
                    found.push((
                        disk.path.display().to_string(),
                        format!(
                            "{} free of {} on the volume of {}",
                            bytes(disk.available),
                            bytes(disk.total),
                            disk.path.display()
                        ),
                    ));
                }
            }
        }
        Condition::CertificateExpiry { within, .. } => {
            for certificate in snapshot.certificates.iter().filter(|c| c.rule == rule.id) {
                let subject = certificate.path.display().to_string();
                match &certificate.not_after {
                    Ok(expiry) => {
                        if expiry.unix_nanos() <= now.unix_nanos() {
                            found.push((
                                subject.clone(),
                                format!("the certificate in {subject} has expired"),
                            ));
                        } else if elapsed(now, *expiry) <= within.0 {
                            found.push((
                                subject.clone(),
                                format!(
                                    "the certificate in {subject} expires in {}",
                                    human(elapsed(now, *expiry))
                                ),
                            ));
                        }
                    }
                    Err(error) => found.push((
                        subject,
                        format!("the certificate cannot be checked: {error}"),
                    )),
                }
            }
        }
    }
    found
}

impl Evaluator {
    /// Evaluates every rule against `snapshot` and returns the
    /// notifications to send.
    pub fn evaluate(&mut self, settings: &AlertSettings, snapshot: &Snapshot) -> Vec<Notification> {
        let Some(now) = snapshot.now else {
            return Vec::new();
        };
        let repeat = settings.repeat.0;
        let mut notifications = Vec::new();
        let mut seen = BTreeSet::new();
        for rule in &settings.rules {
            let hold = rule.hold.map(|d| d.0).unwrap_or_default();
            for (subject, summary) in breaches(rule, snapshot, now) {
                let key = (rule.id.clone(), subject.clone());
                seen.insert(key.clone());
                let state = self.states.entry(key).or_insert_with(|| State {
                    since: now,
                    firing: false,
                    notified_at: None,
                    summary: summary.clone(),
                });
                state.summary = summary;
                let notify = |repeat_notice: bool, state: &State| Notification {
                    rule: rule.id.clone(),
                    kind: rule.condition.kind(),
                    subject: subject.clone(),
                    severity: rule.severity,
                    state: AlertState::Firing,
                    summary: state.summary.clone(),
                    since: state.since,
                    at: now,
                    repeat: repeat_notice,
                    targets: rule.targets.clone(),
                };
                if !state.firing {
                    if elapsed(state.since, now) >= hold {
                        state.firing = true;
                        state.notified_at = Some(now);
                        notifications.push(notify(false, state));
                    }
                } else if !repeat.is_zero()
                    && state
                        .notified_at
                        .is_none_or(|at| elapsed(at, now) >= repeat)
                {
                    state.notified_at = Some(now);
                    notifications.push(notify(true, state));
                }
            }
        }
        let rules: BTreeMap<&str, &RuleConfig> = settings
            .rules
            .iter()
            .map(|rule| (rule.id.as_str(), rule))
            .collect();
        let ended: Vec<(String, String)> = self
            .states
            .keys()
            .filter(|key| !seen.contains(*key))
            .cloned()
            .collect();
        for key in ended {
            let Some(state) = self.states.remove(&key) else {
                continue;
            };
            let Some(rule) = rules.get(key.0.as_str()) else {
                continue;
            };
            if state.firing {
                notifications.push(Notification {
                    rule: key.0.clone(),
                    kind: rule.condition.kind(),
                    subject: key.1.clone(),
                    severity: rule.severity,
                    state: AlertState::Resolved,
                    summary: format!("resolved: {}", state.summary),
                    since: state.since,
                    at: now,
                    repeat: false,
                    targets: rule.targets.clone(),
                });
            }
        }
        notifications
    }

    /// The alerts whose conditions currently hold.
    pub fn active(&self, settings: &AlertSettings) -> Vec<ActiveAlert> {
        let rules: BTreeMap<&str, &RuleConfig> = settings
            .rules
            .iter()
            .map(|rule| (rule.id.as_str(), rule))
            .collect();
        self.states
            .iter()
            .filter_map(|((rule, subject), state)| {
                let config = rules.get(rule.as_str())?;
                Some(ActiveAlert {
                    rule: rule.clone(),
                    kind: config.condition.kind(),
                    subject: subject.clone(),
                    severity: config.severity,
                    summary: state.summary.clone(),
                    since: state.since,
                    firing: state.firing,
                    notified_at: state.notified_at,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use oxim_model::{ChannelId, ConnectorId};
    use oxim_store::QueueStats;

    use super::*;
    use crate::snapshot::{DiskSample, QueueSample};

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000)
    }

    fn settings(yaml: serde_json::Value) -> AlertSettings {
        serde_json::from_value(yaml).unwrap()
    }

    fn queue(depth: u64, now: i64) -> Snapshot {
        Snapshot {
            now: Some(at(now)),
            queues: vec![QueueSample {
                channel: ChannelId::new("lab").unwrap(),
                destination: ConnectorId::new("lis").unwrap(),
                stats: QueueStats {
                    queued: depth,
                    ..QueueStats::default()
                },
            }],
            ..Snapshot::default()
        }
    }

    #[test]
    fn fires_after_the_hold_repeats_and_resolves() {
        let settings = settings(serde_json::json!({
            "repeat": "10m",
            "rules": [{"id": "lis", "kind": "queue_depth", "above": 10, "for": "2m"}]
        }));
        let mut evaluator = Evaluator::default();
        assert!(evaluator.evaluate(&settings, &queue(50, 0)).is_empty());
        assert!(!evaluator.active(&settings)[0].firing);
        assert!(evaluator.evaluate(&settings, &queue(50, 60)).is_empty());
        let fired = evaluator.evaluate(&settings, &queue(60, 120));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].state, AlertState::Firing);
        assert_eq!(fired[0].subject, "lab/lis");
        assert!(
            fired[0].summary.starts_with("60 messages are waiting"),
            "{}",
            fired[0].summary
        );
        assert_eq!(fired[0].since, at(0));
        assert!(evaluator.evaluate(&settings, &queue(60, 300)).is_empty());
        let repeated = evaluator.evaluate(&settings, &queue(60, 720));
        assert!(repeated[0].repeat);
        let resolved = evaluator.evaluate(&settings, &queue(3, 780));
        assert_eq!(resolved[0].state, AlertState::Resolved);
        assert!(evaluator.active(&settings).is_empty());
        // A breach that ends before its hold never notifies.
        assert!(evaluator.evaluate(&settings, &queue(50, 800)).is_empty());
        assert!(evaluator.evaluate(&settings, &queue(1, 860)).is_empty());
    }

    #[test]
    fn checks_disks_by_bytes_and_percent() {
        let settings = settings(serde_json::json!({
            "rules": [
                {"id": "pct", "kind": "disk_space", "below": "10%"},
                {"id": "abs", "kind": "disk_space", "below": "1GiB"}
            ]
        }));
        let disk = |rule: &str, available: u64| DiskSample {
            rule: rule.into(),
            path: "/var/lib/oxim".into(),
            available,
            total: 100 << 30,
        };
        let snapshot = Snapshot {
            now: Some(at(0)),
            disks: vec![disk("pct", 5 << 30), disk("abs", 5 << 30)],
            ..Snapshot::default()
        };
        let fired = Evaluator::default().evaluate(&settings, &snapshot);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].rule, "pct");
        assert!(
            fired[0].summary.contains("5.0 GiB free of 100.0 GiB"),
            "{}",
            fired[0].summary
        );
    }

    #[test]
    fn formats_durations() {
        assert_eq!(human(Duration::from_secs(45)), "45s");
        assert_eq!(human(Duration::from_secs(720)), "12m");
        assert_eq!(human(Duration::from_secs(3 * 3600 + 300)), "3h 5m");
        assert_eq!(human(Duration::from_secs(2 * 86_400)), "2d");
    }
}
